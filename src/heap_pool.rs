use std::rc::Rc;
use std::cell;

struct PoolState {
    bufs: Vec<Vec<u8>>,
    packet_size: usize,
}

pub struct HeapPool {
    state: Rc<cell::RefCell<PoolState>>
}
impl HeapPool {
    pub fn new(packet_count: usize, packet_size: usize) -> HeapPool {
        HeapPool {
            state: Rc::new(cell::RefCell::new(PoolState {
                bufs: Self::mk_bufs(packet_count, packet_size),
                packet_size,
            }))
        }
    }

    fn mk_bufs(packet_count: usize, packet_size: usize) -> Vec<Vec<u8>> {
        (0..packet_count).map(|_| Vec::with_capacity(packet_size) ).collect()
    }
}
impl crate::BufferPool for HeapPool {
    type P = HeapPacket;

    fn allocate(&self) -> Option<Self::P> {
        // TODO: maybe this should allocate N at once, for efficiency?

        let mut state = self.state.borrow_mut();
        let buf = state.bufs.pop();
        if buf.is_none() {
            return None;
        }
        let mut buf = buf.unwrap();
        // TODO: try to cook up a scheme to avoid zeroing-out the buffer
        //       e.g. https://github.com/tbu-/buffer ?
        buf.clear();
        buf.resize(state.packet_size, 0);
        Some(HeapPacket {
            state: self.state.clone(),
            buf,
        })
    }
}
impl Clone for HeapPool {
    fn clone(&self) -> Self {
        HeapPool {
            state: self.state.clone(),
        }
    }
}

pub struct HeapPacket {
    state: Rc<cell::RefCell<PoolState>>,
    buf: Vec<u8>,
}
impl crate::Packet for HeapPacket {
    type R = HeapPacketRef;
    type W = HeapPacketRefMut;

    fn into_ref(self) -> Self::R {
        HeapPacketRef {
            pk: Rc::new(self),
        }
    }

    fn into_ref_mut(self) -> Self::W {
        HeapPacketRefMut {
            pk: self,
        }
    }
}
impl Drop for HeapPacket {
    fn drop(&mut self) {
        self.state.borrow_mut().bufs.push(self.buf.clone())
    }
}

#[derive(Clone)]
pub struct HeapPacketRef {
    pk: Rc<HeapPacket>,
}
impl crate::PacketRef for HeapPacketRef {
    type P = HeapPacket;

    fn payload(&self) -> &[u8] {
        &self.pk.buf[..]
    }

    fn try_into_packet(self) -> Result<HeapPacket, HeapPacketRef> {
        Rc::try_unwrap(self.pk)
            .map_err(|e| HeapPacketRef { pk: e })
    }
}

pub struct HeapPacketRefMut {
    pk: HeapPacket,
}
impl crate::PacketRefMut for HeapPacketRefMut {
    type P = HeapPacket;

    fn payload(&mut self) -> &mut[u8] {
        &mut self.pk.buf[..]
    }

    fn into_packet(self) -> Self::P {
        self.pk
    }

    fn truncate(&mut self, size: usize) {
        assert_ne!(size, 0);
        assert!(self.pk.buf.len() >= size);
        self.pk.buf.truncate(size)
    }
}

#[cfg(test)]
mod test {
    use crate::heap_pool::HeapPool;
    use crate::BufferPool;
    use crate::Packet;
    use crate::PacketRefMut;
    use crate::PacketRef;

    #[test]
    fn it_works() {
        let pool = HeapPool::new(2, 1500);
        {
            let one = pool.allocate().unwrap();
            let two = pool.allocate().unwrap();
            assert!(pool.allocate().is_none());
            let one_ref = {
                let one_ref = one.into_ref();
                let one_ref_b = one_ref.clone();
                // will not be able to get the packet back again while one_ref_b also exits
                let res = one_ref.try_into_packet();
                assert!(res.is_err());
                res.err().unwrap()
            };
            // now ref_b has been dropped, try_into_packet() should succeed
            assert!(one_ref.try_into_packet().is_ok());

            let mut two_ref = two.into_ref_mut();
            {
                let data = two_ref.payload();
                data[0] = 123;
            }
            let two = two_ref.into_packet();
            let two_ref = two.into_ref();
            assert_eq!(123, two_ref.payload()[0]);
        }
        // Now all the above pool usage is done, the allocations should have returned to the pool
        let one = pool.allocate().unwrap();
        let two = pool.allocate().unwrap();
        assert!(pool.allocate().is_none());
    }
}