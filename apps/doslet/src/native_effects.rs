//! Flatten effects until the presentation owner can submit them to iced.
use iced::Task;

pub(crate) struct Effects<T>(Vec<Task<T>>);
impl<T> Default for Effects<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T: 'static> Effects<T> {
    pub fn push(&mut self, task: Task<T>) {
        // Task::batch([none, none]) still owns an empty stream. Wrapping it
        // repeatedly nests SelectAll even though its unit count remains zero.
        if task.units() != 0 {
            self.0.push(task);
        }
    }
    pub fn append(&mut self, other: Self) {
        self.0.extend(other.0);
    }
    pub fn into_task(mut self) -> Task<T> {
        match self.0.len() {
            0 => Task::none(),
            1 => self.0.pop().unwrap(),
            _ => Task::batch(self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{StreamExt, executor::block_on};

    #[test]
    fn stalled_presentation_keeps_effects_flat_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut effects = Effects::default();
                for i in 0..100_000 {
                    let mut incoming = Effects::default();
                    incoming.push(Task::batch([Task::none(), Task::none()]));
                    if i % 1000 == 0 {
                        incoming.push(Task::done(i));
                    }
                    effects.append(incoming);
                }
                assert_eq!(effects.0.len(), 100);
                let mut stream = iced_runtime::task::into_stream(effects.into_task()).unwrap();
                let mut outputs = Vec::new();
                block_on(async {
                    while let Some(action) = stream.next().await {
                        match action {
                            iced_runtime::Action::Output(value) => outputs.push(value),
                            _ => panic!("unexpected effect"),
                        }
                    }
                });
                outputs.sort_unstable();
                assert_eq!(outputs, (0..100_000).step_by(1000).collect::<Vec<_>>());
                let mut empty = Effects::<()>::default();
                for _ in 0..100_000 {
                    empty.push(Task::none());
                }
                assert!(iced_runtime::task::into_stream(empty.into_task()).is_none());
                // Shutdown/cancellation must also drop a long pending queue
                // without recursively dropping one batch per owner turn.
                let mut cancelled = Effects::default();
                for i in 0..10_000 {
                    cancelled.push(Task::done(i));
                }
                drop(cancelled.into_task());
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
