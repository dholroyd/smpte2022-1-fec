use log::*;
use mpeg2ts_reader::demux_context;
use mpeg2ts_reader::packet_filter_switch;
use mpeg2ts_reader::{demultiplex, packet, pes};
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

packet_filter_switch! {
    CheckFilterSwitch<CheckDemuxContext> {
        Pat: demultiplex::PatPacketFilter<CheckDemuxContext>,
        Pmt: demultiplex::PmtPacketFilter<CheckDemuxContext>,
        Pes: pes::PesPacketFilter<CheckDemuxContext, NullElementaryStreamConsumer>,
        Null: demultiplex::NullPacketFilter<CheckDemuxContext>,
    }
}
demux_context!(CheckDemuxContext, CheckFilterSwitch);
impl CheckDemuxContext {
    fn do_construct(&mut self, req: demultiplex::FilterRequest<'_, '_>) -> CheckFilterSwitch {
        match req {
            demultiplex::FilterRequest::ByPid(packet::Pid::PAT) => {
                CheckFilterSwitch::Pat(demultiplex::PatPacketFilter::default())
            }
            demultiplex::FilterRequest::ByPid(packet::Pid::STUFFING) => {
                CheckFilterSwitch::Null(demultiplex::NullPacketFilter::default())
            }
            demultiplex::FilterRequest::ByPid(_) => {
                CheckFilterSwitch::Null(demultiplex::NullPacketFilter::default())
            }
            demultiplex::FilterRequest::ByStream {
                stream_type,
                pmt: _,
                stream_info,
                ..
            } => {
                if stream_type.is_pes() {
                    println!(
                        "adding {:?} {:?}",
                        stream_type,
                        stream_info.elementary_pid()
                    );
                    CheckFilterSwitch::Pes(pes::PesPacketFilter::new(NullElementaryStreamConsumer))
                } else {
                    CheckFilterSwitch::Null(demultiplex::NullPacketFilter::default())
                }
            }
            demultiplex::FilterRequest::Pmt {
                pid,
                program_number,
            } => CheckFilterSwitch::Pmt(demultiplex::PmtPacketFilter::new(pid, program_number)),
            demultiplex::FilterRequest::Nit { .. } => {
                CheckFilterSwitch::Null(demultiplex::NullPacketFilter::default())
            }
        }
    }
}
pub struct NullElementaryStreamConsumer;
impl<Ctx> pes::ElementaryStreamConsumer<Ctx> for NullElementaryStreamConsumer {
    fn start_stream(&mut self, _ctx: &mut Ctx) {}
    fn begin_packet(&mut self, _ctx: &mut Ctx, _header: pes::PesHeader) {}
    fn continue_packet(&mut self, _ctx: &mut Ctx, _data: &[u8]) {}
    fn end_packet(&mut self, _ctx: &mut Ctx) {}
    fn continuity_error(&mut self, _ctx: &mut Ctx) {}
}

struct MyReceiver {
    last_seq: Option<rtp_rs::Seq>,
    stats: rc::Rc<cell::RefCell<Stats>>,
    demux: demultiplex::Demultiplex<CheckDemuxContext>,
    ctx: CheckDemuxContext,
}
impl MyReceiver {
    fn new(stats: rc::Rc<cell::RefCell<Stats>>) -> MyReceiver {
        let mut ctx = CheckDemuxContext::new();

        // create the demultiplexer, which will use the ctx to create a filter for pid 0 (PAT)
        let demux = demultiplex::Demultiplex::new(&mut ctx);
        MyReceiver {
            last_seq: None,
            stats,
            demux,
            ctx,
        }
    }
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
                    if header.payload().len() % packet::Packet::SIZE == 0 {
                        self.demux.push(&mut self.ctx, header.payload());
                    } else {
                        println!(
                            "Ignoring packet with suspicious payload len {}",
                            header.payload().len()
                        );
                    }
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
    Ok(mio::net::UdpSocket::from_std(s.into_udp_socket()))
}

fn main() -> Result<(), std::io::Error> {
    env_logger::init();

    let stats = rc::Rc::new(cell::RefCell::new(Stats {
        packets: 0,
        losses: 0,
        recovered: 0,
    }));
    let base_port = 5000;
    let mut main_sock = create_source(base_port)?;
    let mut fec_one = create_source(base_port + 2)?;
    let mut fec_two = create_source(base_port + 4)?;

    let buffer_pool = HeapPool::new(PACKET_COUNT_MAX, PACKET_SIZE_MAX);
    let recv = MyReceiver::new(stats);
    let mut decoder = Decoder::new(buffer_pool.clone(), recv);

    let mut poll = mio::Poll::new()?;
    let reg = poll.registry();
    reg.register(
        &mut main_sock,
        MAIN,
        mio::Interest::READABLE,
    )?;
    reg.register(
        &mut fec_one,
        FEC_ONE,
        mio::Interest::READABLE,
    )?;
    reg.register(
        &mut fec_two,
        FEC_TWO,
        mio::Interest::READABLE,
    )?;

    let mut events = mio::Events::with_capacity(1024);
    let mut pk_buf = Vec::new();
    loop {
        poll.poll(&mut events, Some(time::Duration::from_secs(1)))?;
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
                                .unwrap_or_else(|e| error!("Main packet: {:?}", e))
                        }
                    }
                    decoder
                        .add_main_packets(pk_buf.drain(..))
                        .unwrap_or_else(|e| error!("Main packet: {:?}", e))
                }
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
                                .unwrap_or_else(|e| error!("Col packet: {:?}", e))
                        }
                    }
                    decoder
                        .add_column_packets(pk_buf.drain(..))
                        .unwrap_or_else(|e| error!("Col packet: {:?}", e))
                }
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
                                .unwrap_or_else(|e| error!("Row packet: {:?}", e))
                        }
                    }
                    decoder
                        .add_row_packets(pk_buf.drain(..))
                        .unwrap_or_else(|e| error!("Row packet: {:?}", e))
                }
                t => panic!("unexpected {:?}", t),
            }
        }
    }
}
