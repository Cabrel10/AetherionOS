// Aetherion OS - Inter-Process Communication
// Phase 2.4: IPC mechanisms

use crate::process::Pid;
use alloc::vec::Vec;
use spin::Mutex;

/// Message for IPC
#[derive(Clone)]
pub struct Message {
    sender: Pid,
    data: Vec<u8>,
}

impl Message {
    pub fn new(sender: Pid, data: Vec<u8>) -> Self {
        Message { sender, data }
    }
    
    pub fn sender(&self) -> Pid {
        self.sender
    }
    
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Message queue for a process
struct MessageQueue {
    messages: Vec<Message>,
    capacity: usize,
}

impl MessageQueue {
    fn new(capacity: usize) -> Self {
        MessageQueue {
            messages: Vec::with_capacity(capacity),
            capacity,
        }
    }
    
    fn send(&mut self, msg: Message) -> Result<(), &'static str> {
        if self.messages.len() >= self.capacity {
            return Err("Queue full");
        }
        self.messages.push(msg);
        Ok(())
    }
    
    fn receive(&mut self) -> Option<Message> {
        if self.messages.is_empty() {
            None
        } else {
            Some(self.messages.remove(0))
        }
    }
}

/// Global IPC system
static IPC_QUEUES: Mutex<[Option<MessageQueue>; 256]> = Mutex::new([const { None }; 256]);

/// Initialize IPC for a process
pub fn init_process_queue(pid: Pid, capacity: usize) {
    let mut queues = IPC_QUEUES.lock();
    let index = pid.as_usize() % 256;
    queues[index] = Some(MessageQueue::new(capacity));
}

/// Send a message to a process
pub fn send(to: Pid, from: Pid, data: Vec<u8>) -> Result<(), &'static str> {
    let msg = Message::new(from, data);
    let mut queues = IPC_QUEUES.lock();
    let index = to.as_usize() % 256;
    
    match queues[index].as_mut() {
        Some(queue) => queue.send(msg),
        None => Err("No queue for recipient"),
    }
}

/// Receive a message
pub fn receive(pid: Pid) -> Option<Message> {
    let mut queues = IPC_QUEUES.lock();
    let index = pid.as_usize() % 256;
    
    queues[index].as_mut()?.receive()
}

pub fn init() {
    // IPC system initialized
}
