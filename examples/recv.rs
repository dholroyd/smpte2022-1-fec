use smpte2022_1_fec::heap_pool::HeapPacket;
use smpte2022_1_fec::heap_pool::HeapPool;
use smpte2022_1_fec::*;
use socket2::{Domain, Protocol, Socket, Type};
use std::cell;
use std::io;
use std::net::SocketAddr;
use std::rc;
use std::time;

const MAIN: mio::Token = mio::Token(0);
const FEC_ONE: mio::Token = mio::Token(1);
const FEC_TWO: mio::Token = mio::Token(2);
const TIMER: mio::Token = mio::Token(3);

const PACKET_SIZE_MAX: usize = 1500;
const PACKET_COUNT_MAX: usize = 10 * 10 * 2;
const MAX_PACKET_BATCH: usize = 10;

struct Stats {
    packets: u64,
    losses: u64,
    recovered: u64,
}
impl Stats {
    fn dump(&self) {
        println!(
            "RTP: received={} uncorrectable={} corrected={}",
            self.packets, self.losses, self.recovered
        );
    }
}

struct MyReceiver {
    last_seq: Option<rtp_rs::Seq>,
    stats: rc::Rc<cell::RefCell<Stats>>,
}
impl Receiver<HeapPacket> for MyReceiver {
    fn receive(&mut self, packets: impl Iterator<Item = (HeapPacket, PacketStatus)>) {
        let mut stats = self.stats.borrow_mut();
        for (pk, pk_status) in packets {
            stats.packets += 1;
            if pk_status == PacketStatus::Recovered {
                stats.recovered += 1;
            }
            match rtp_rs::RtpReader::new(pk.payload()) {
                Ok(header) => {
                    let this_seq = header.sequence_number();
                    if let Some(last) = self.last_seq {
                        if !last.precedes(this_seq) {
                            let diff = this_seq - last;
                            if diff > 0 {
                                // if this_seq = 5, and last = 3, then diff will be '2', but
                                // actually we only lost a single packet (the one with seq=4),
                                // hence we subtract 1 here,
                                stats.losses += diff as u64 - 1;
                                println!(
                                    "Lost {} packets between {:?} and {:?}",
                                    diff - 1,
                                    last,
                                    this_seq
                                );
                            } else {
                                // If 'diff' becomes negative, that indicates  a very large gap
                                // in sequence numbers could well mean that the sender has reset
                                // it's sequence, or that the source has been down for so long
                                // that there may have been sequence number wrap-arounds (and
                                // without knowledge of the packet-rate we can't estimate this).
                                // Therefore we just don't update the packet loss counter in this
                                // case.
                                println!(
                                    "Sequence number change of {} from {:?} to {:?}",
                                    diff, last, this_seq
                                );
                            }
                        }
                    }
                    self.last_seq = Some(this_seq);
                }
                Err(e) => println!("packet error {:?}", e),
            }
        }
    }
}

fn create_source(port: u16) -> Result<mio::net::UdpSocket, io::Error> {
    let s = Socket::new(Domain::ipv4(), Type::dgram(), Some(Protocol::udp()))?;
    s.set_recv_buffer_size(2 * 1024 * 1024)?;
    let addr = SocketAddr::new(
        "127.0.0.1"
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{:?}", e)))?,
        port,
    );
    s.bind(&addr.into())?;
    mio::net::UdpSocket::from_socket(s.into_udp_socket())
}

fn main() -> Result<(), std::io::Error> {
    env_logger::init();

    let stats = rc::Rc::new(cell::RefCell::new(Stats {
        packets: 0,
        losses: 0,
        recovered: 0,
    }));
    let base_port = 5000;
    let main_sock = create_source(base_port)?;
    let fec_one = create_source(base_port + 2)?;
    let fec_two = create_source(base_port + 4)?;
    let mut timer = mio_extras::timer::Timer::default();
    timer.set_timeout(time::Duration::from_millis(2000), stats.clone());

    let buffer_pool = HeapPool::new(PACKET_COUNT_MAX, PACKET_SIZE_MAX);
    let recv = MyReceiver {
        last_seq: None,
        stats,
    };
    let mut decoder = Decoder::new(buffer_pool.clone(), recv);

    let poll = mio::Poll::new()?;
    poll.register(&timer, TIMER, mio::Ready::readable(), mio::PollOpt::edge())?;
    poll.register(
        &main_sock,
        MAIN,
        mio::Ready::readable(),
        mio::PollOpt::edge(),
    )?;
    poll.register(
        &fec_one,
        FEC_ONE,
        mio::Ready::readable(),
        mio::PollOpt::edge(),
    )?;
    poll.register(
        &fec_two,
        FEC_TWO,
        mio::Ready::readable(),
        mio::PollOpt::edge(),
    )?;

    let mut events = mio::Events::with_capacity(1024);
    let mut pk_buf = Vec::new();
    loop {
        poll.poll(&mut events, None)?;
        for event in &events {
            match event.token() {
                MAIN => {
                    loop {
                        let mut pk = buffer_pool.allocate().expect("allocating main buffer");
                        let size = match main_sock.recv(pk.payload_mut()) {
                            Ok(s) => s,
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                break;
                            }
                            e => panic!("err={:?}", e),
                        };
                        pk.truncate(size);
                        pk_buf.push(pk);
                        if pk_buf.len() > MAX_PACKET_BATCH {
                            decoder
                                .add_main_packets(pk_buf.drain(..))
                                .expect("decoding main packet");
                        }
                    }
                    decoder
                        .add_main_packets(pk_buf.drain(..))
                        .expect("decoding main packet");
                },
                FEC_ONE => {
                    loop {
                        let mut pk = buffer_pool.allocate().expect("allocating column buffer");
                        let size = match fec_one.recv(pk.payload_mut()) {
                            Ok(s) => s,
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                break;
                            }
                            e => panic!("err={:?}", e),
                        };
                        pk.truncate(size);
                        pk_buf.push(pk);
                        if pk_buf.len() > MAX_PACKET_BATCH {
                            decoder
                                .add_column_packets(pk_buf.drain(..))
                                .expect("decoding column packet");
                        }
                    }
                    decoder
                        .add_column_packets(pk_buf.drain(..))
                        .expect("decoding column packet");
                },
                FEC_TWO => {
                    loop {
                        let mut pk = buffer_pool.allocate().expect("allocating row buffer");
                        let size = match fec_two.recv(pk.payload_mut()) {
                            Ok(s) => s,
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                break;
                            }
                            e => panic!("err={:?}", e),
                        };
                        pk.truncate(size);
                        pk_buf.push(pk);
                        if pk_buf.len() > MAX_PACKET_BATCH {
                            decoder
                                .add_row_packets(pk_buf.drain(..))
                                .expect("decoding row packet");
                        }
                    }
                    decoder
                        .add_row_packets(pk_buf.drain(..))
                        .expect("decoding row packet");
                },
                TIMER => {
                    let stats = timer.poll().unwrap();
                    stats.borrow().dump();
                    // re-arm the timeout,
                    timer.set_timeout(time::Duration::from_millis(2000), stats);
                }
                t => panic!("unexpected {:?}", t),
            }
        }
    }
}
