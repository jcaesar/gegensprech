use tokio::sync::mpsc;

pub trait UndoOnDrop {}
pub struct CallOnDrop<T: FnOnce()>(Option<T>);

impl<F: FnOnce()> CallOnDrop<F> {
	pub fn call(f: F) -> Self {
		Self(Some(f))
	}
}

impl<T: FnOnce()> UndoOnDrop for CallOnDrop<T> {}

impl<T: FnOnce()> Drop for CallOnDrop<T> {
	fn drop(&mut self) {
		self.0.take().unwrap()();
	}
}

pub fn keep_alive<T>(channel: &mpsc::Sender<T>) {
	Box::leak(Box::new(channel.clone()));
}
