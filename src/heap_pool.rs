use std::cell;
use std::rc::Rc;

struct PoolState {
    bufs: Vec<Vec<u8>>,
    packet_size: usize,
}

pub struct HeapPool {
    state: Rc<cell::RefCell<PoolState>>,
}
impl HeapPool {
    pub fn new(packet_count: usize, packet_size: usize) -> HeapPool {
        HeapPool {
            state: Rc::new(cell::RefCell::new(PoolState {
                bufs: Self::mk_bufs(packet_count, packet_size),
                packet_size,
            })),
        }
    }

    fn mk_bufs(packet_count: usize, packet_size: usize) -> Vec<Vec<u8>> {
        (0..packet_count)
            .map(|_| Vec::with_capacity(packet_size))
            .collect()
    }
}
impl crate::BufferPool for HeapPool {
    type P = HeapPacket;

    fn allocate(&self) -> Option<Self::P> {
        // TODO: maybe this should allocate N at once, for efficiency?

        let mut state = self.state.borrow_mut();
        let mut buf = state.bufs.pop()?;
        // TODO: try to cook up a scheme to avoid zeroing-out the buffer
        //       e.g. https://github.com/tbu-/buffer ?
        buf.clear();
        buf.resize(state.packet_size, 0);
        Some(HeapPacket {
            state: self.state.clone(),
            buf: Some(buf),
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
    // the Vec is wrapped in an Option simply so that we can extract it and pass it by value back
    // to the Heap pool in the Drop impl.  buf will be 'Some' at all other times.
    buf: Option<Vec<u8>>,
}
impl crate::Packet for HeapPacket {
    type R = HeapPacketRef;
    type W = HeapPacketRefMut;

    fn into_ref(self) -> Self::R {
        HeapPacketRef { pk: self }
    }

    fn into_ref_mut(self) -> Self::W {
        HeapPacketRefMut { pk: self }
    }
}
impl Drop for HeapPacket {
    fn drop(&mut self) {
        self.state.borrow_mut().bufs.push(self.buf.take().unwrap())
    }
}

pub struct HeapPacketRef {
    pk: HeapPacket,
}
impl crate::PacketRef for HeapPacketRef {
    type P = HeapPacket;

    fn payload(&self) -> &[u8] {
        &self.pk.buf.as_ref().unwrap()[..]
    }

    fn try_into_packet(self) -> Result<HeapPacket, HeapPacketRef> {
        Ok(self.pk)
    }
}

pub struct HeapPacketRefMut {
    pk: HeapPacket,
}
impl crate::PacketRefMut for HeapPacketRefMut {
    type P = HeapPacket;

    fn payload(&mut self) -> &mut [u8] {
        &mut self.pk.buf.as_mut().unwrap()[..]
    }

    fn into_packet(self) -> Self::P {
        self.pk
    }

    fn truncate(&mut self, size: usize) {
        assert_ne!(size, 0);
        assert!(self.pk.buf.as_ref().unwrap().len() >= size);
        self.pk.buf.as_mut().unwrap().truncate(size)
    }
}

#[cfg(test)]
mod test {
    use crate::heap_pool::HeapPool;
    use crate::BufferPool;
    use crate::Packet;
    use crate::PacketRef;
    use crate::PacketRefMut;

    #[test]
    fn it_works() {
        let pool = HeapPool::new(2, 1500);
        {
            let one = pool.allocate().unwrap();
            let two = pool.allocate().unwrap();
            assert!(pool.allocate().is_none());
            let one_ref = one.into_ref();
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
