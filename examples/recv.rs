use std::io;
use socket2::{Socket, Domain, Type, Protocol};
use std::net::SocketAddr;
use smpte2022_1_fec::*;
use smpte2022_1_fec::heap_pool::HeapPool;

const MAIN: mio::Token = mio::Token(0);
const FEC_ONE: mio::Token = mio::Token(1);
const FEC_TWO: mio::Token = mio::Token(2);

const PACKET_SIZE_MAX: usize = 1500;
const PACKET_COUNT_MAX: usize = 10 * 10 * 2;

fn create_source(port: u16) -> Result<mio::net::UdpSocket, io::Error> {
    let s = Socket::new(Domain::ipv4(), Type::dgram(), Some(Protocol::udp()))?;
    s.set_recv_buffer_size(2*1024*1024)?;
    let addr = SocketAddr::new("127.0.0.1".parse().map_err(|e| io::Error::new(io::ErrorKind::Other, format!("{:?}",e)))?, port);
    s.bind(&addr.into())?;
    mio::net::UdpSocket::from_socket(s.into_udp_socket())
}

fn main() -> Result<(), std::io::Error> {
    let base_port = 5000;
    let main_sock = create_source(base_port)?;
    let fec_one = create_source(base_port + 2)?;
    let fec_two = create_source(base_port + 4)?;

    let buffer_pool = HeapPool::new(PACKET_COUNT_MAX, PACKET_SIZE_MAX);
    let mut decoder = Decoder::new(buffer_pool.clone());

    let poll = mio::Poll::new()?;
    poll.register(&main_sock, MAIN, mio::Ready::readable(), mio::PollOpt::edge())?;
    poll.register(&fec_one, FEC_ONE, mio::Ready::readable(), mio::PollOpt::edge())?;
    poll.register(&fec_two, FEC_TWO, mio::Ready::readable(), mio::PollOpt::edge())?;

    let mut events = mio::Events::with_capacity(1024);
    loop {
        poll.poll(&mut events, None)?;
        for event in &events {
            match event.token() {
                MAIN => loop {
                    let pk = buffer_pool.allocate().expect("allocating main buffer");
                    let mut ref_mut = pk.into_ref_mut();
                    let size = match main_sock.recv(ref_mut.payload()) {
                        Ok(s) => s,
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            break;
                        },
                        e => panic!("err={:?}", e),
                    };
                    ref_mut.truncate(size);
                    decoder.add_main_packets(vec![ref_mut.into_packet()].into_iter())
                        .expect("decoding main packet");
                },
                FEC_ONE => loop {
                    let pk = buffer_pool.allocate().expect("allocating main buffer");
                    let mut ref_mut = pk.into_ref_mut();
                    let size = match fec_one.recv(ref_mut.payload()) {
                        Ok(s) => s,
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            break;
                        },
                        e => panic!("err={:?}", e),
                    };
                    ref_mut.truncate(size);
                    decoder.add_column_packets(vec![ref_mut.into_packet()].into_iter())
                        .expect("decoding column packet");
                },
                FEC_TWO => loop {
                    let pk = buffer_pool.allocate().expect("allocating main buffer");
                    let mut ref_mut = pk.into_ref_mut();
                    println!("FEC_TWO len {}", ref_mut.payload().len());
                    let size = match fec_two.recv(ref_mut.payload()) {
                        Ok(s) => s,
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                            break;
                        },
                        e => panic!("err={:?}", e),
                    };
                    ref_mut.truncate(size);
                    decoder.add_row_packets(vec![ref_mut.into_packet()].into_iter())
                        .expect("decoding row packet");
                },
                t => panic!("unexpected {:?}", t),
            }
        }
    }
}
