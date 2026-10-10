use std::any::Any;

/// The working memory one caller keeps for one gain model.
///
/// A gain model is shared (`&self`, `Sync`): the render thread and the workers
/// of a table build all evaluate the same one. Whatever a model needs beyond
/// the gains it writes — a second gain set to blend with, the arrays of an
/// iterative solver — it therefore cannot hold itself, and its size depends on
/// the layout, so it cannot sit on the stack either. The model describes it
/// once, in [`GainModel::new_scratch`](super::GainModel::new_scratch), and
/// every caller hands its own back on each
/// [`compute_gains`](super::GainModel::compute_gains).
///
/// Sized for the layout when it is made, off the render thread: nothing is
/// allocated while gains are computed.
pub struct GainScratch(Box<dyn Any + Send>);

impl GainScratch {
    /// The scratch of a model that needs none. Allocates nothing.
    pub fn none() -> Self {
        Self(Box::new(()))
    }

    /// A scratch holding `state`, the working memory of the model making it.
    pub fn new<T: Any + Send>(state: T) -> Self {
        Self(Box::new(state))
    }

    /// The state put in by [`new`](Self::new), or `None` when this scratch
    /// was made by a model of another kind. A model handed such a scratch
    /// answers silence ([`foreign_scratch`]) rather than panic.
    #[inline]
    pub fn state<T: Any + Send>(&mut self) -> Option<&mut T> {
        self.0.downcast_mut()
    }
}

/// What a model answers when the scratch it is handed is not one it made:
/// silence, the best effort the hot-path contract asks for where a panic would
/// take the render thread down. A debug build stops on it: the caller paired a
/// model with another model's scratch.
#[cold]
pub fn foreign_scratch(out: &mut [f32]) {
    debug_assert!(false, "gain model evaluated with a scratch it did not make");
    out.fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_gives_back_the_state_it_was_made_with_and_no_other() {
        let mut scratch = GainScratch::new(vec![1.0f32, 2.0]);
        assert_eq!(scratch.state::<Vec<f32>>(), Some(&mut vec![1.0f32, 2.0]));
        assert!(scratch.state::<Vec<f64>>().is_none());
        assert!(GainScratch::none().state::<Vec<f32>>().is_none());
    }
}
