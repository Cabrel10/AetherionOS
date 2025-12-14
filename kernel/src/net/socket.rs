// Socket API
pub enum SocketType {
    Stream,
    Datagram,
}

pub struct Socket {
    socket_type: SocketType,
    local_port: u16,
    remote_addr: [u8; 4],
    remote_port: u16,
}

impl Socket {
    pub fn new(socket_type: SocketType) -> Self {
        Socket {
            socket_type,
            local_port: 0,
            remote_addr: [0; 4],
            remote_port: 0,
        }
    }
    
    pub fn bind(&mut self, port: u16) -> Result<(), &'static str> {
        self.local_port = port;
        Ok(())
    }
    
    pub fn connect(&mut self, addr: [u8; 4], port: u16) -> Result<(), &'static str> {
        self.remote_addr = addr;
        self.remote_port = port;
        Ok(())
    }
    
    pub fn send(&self, _data: &[u8]) -> Result<usize, &'static str> {
        Ok(0)
    }
    
    pub fn recv(&self, _buffer: &mut [u8]) -> Result<usize, &'static str> {
        Ok(0)
    }
}
