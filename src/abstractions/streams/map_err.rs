use abstractions::streams::stream::Stream;
use abstractions::poll::Poll;

#[must_use = "streams do nothing unless polled"]
pub struct MapErr<S, F> {
    stream: S,
    f: F,
}

pub fn new<S, F, U>(s: S, f: F) -> MapErr<S, F>
    where S: Stream,
          F: FnMut(S::Error) -> U
{
    MapErr { stream: s, f: f }
}

impl<S, F, U> Stream for MapErr<S, F>
    where S: Stream,
          F: FnMut(S::Error) -> U
{
    type Item = S::Item;
    type Error = U;

    fn poll(&mut self) -> Poll<Option<S::Item>, U> {
        self.stream.poll().map_err(&mut self.f)
    }
}
