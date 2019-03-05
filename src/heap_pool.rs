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
    fn payload(&self) -> &[u8] {
        &self.buf.as_ref().unwrap()[..]
    }
    fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.buf.as_mut().unwrap()[..]
    }
    fn truncate(&mut self, size: usize) {
        assert_ne!(size, 0);
        assert!(self.buf.as_ref().unwrap().len() >= size);
        self.buf.as_mut().unwrap().truncate(size)
    }
}
impl Drop for HeapPacket {
    fn drop(&mut self) {
        self.state.borrow_mut().bufs.push(self.buf.take().unwrap())
    }
}

#[cfg(test)]
mod test {
    use crate::heap_pool::HeapPool;
    use crate::BufferPool;
    use crate::Packet;

    #[test]
    fn it_works() {
        let pool = HeapPool::new(2, 1500);
        {
            let one = pool.allocate().unwrap();
            let mut two = pool.allocate().unwrap();
            assert!(pool.allocate().is_none());
            {
                let data = two.payload_mut();
                data[0] = 123;
            }
            assert_eq!(123, two.payload()[0]);
        }
        // Now all the above pool usage is done, the allocations should have returned to the pool
        let one = pool.allocate().unwrap();
        let two = pool.allocate().unwrap();
        assert!(pool.allocate().is_none());
    }
}
