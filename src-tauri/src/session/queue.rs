//! Queue entries belong to the transport generation on which they were requested.
use tokio::sync::{mpsc, watch};

pub(super) struct Message<T> {
    generation: u64,
    body: T,
}
pub(super) struct Sender<T> {
    sender: mpsc::Sender<Message<T>>,
    generation: watch::Sender<u64>,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            generation: self.generation.clone(),
        }
    }
}
pub(super) struct Permit<'a, T> {
    permit: mpsc::Permit<'a, Message<T>>,
    generation: u64,
}
impl<T> Sender<T> {
    pub fn channel(capacity: usize) -> (Self, mpsc::Receiver<Message<T>>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            Self {
                sender,
                generation: watch::channel(0).0,
            },
            receiver,
        )
    }
    pub fn generation(&self) -> u64 {
        *self.generation.borrow()
    }
    pub fn advance(&self) {
        self.generation.send_modify(|generation| *generation += 1);
    }
    pub async fn send(&self, body: T) -> Result<(), mpsc::error::SendError<()>> {
        let mut changed = self.generation.subscribe();
        let generation = *changed.borrow_and_update();
        tokio::select! {
            biased;
            _ = changed.changed() => Err(mpsc::error::SendError(())),
            sent = self.sender.send(Message { generation, body }) => sent.map_err(|_| mpsc::error::SendError(())),
        }
    }
    pub async fn reserve(&self) -> Result<Permit<'_, T>, mpsc::error::SendError<()>> {
        let mut changed = self.generation.subscribe();
        let generation = *changed.borrow_and_update();
        let permit = tokio::select! {
            biased;
            _ = changed.changed() => return Err(mpsc::error::SendError(())),
            permit = self.sender.reserve() => permit?,
        };
        if generation != self.generation() {
            return Err(mpsc::error::SendError(()));
        }
        Ok(Permit { permit, generation })
    }
}
impl<T> Permit<'_, T> {
    pub fn send(self, body: T) {
        self.permit.send(Message {
            generation: self.generation,
            body,
        });
    }
}
pub(super) async fn receive<T>(
    receiver: &mut mpsc::Receiver<Message<T>>,
    generation: u64,
) -> Option<T> {
    while let Some(message) = receiver.recv().await {
        if message.generation == generation {
            return Some(message.body);
        }
    }
    None
}
