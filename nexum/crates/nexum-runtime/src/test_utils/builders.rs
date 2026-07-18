//! Pass-through [`ComponentBuilder`] wrapping a pre-built backend, so a test
//! hands a programmed mock straight into the public builder path.

use crate::host::component::{BuilderContext, ComponentBuilder};

/// A [`ComponentBuilder`] that yields a pre-built backend, ignoring the build
/// context. Wrap any mock instance (chain, store, or extension payload) to
/// compose it through [`RuntimeBuilder::with_components`].
///
/// [`RuntimeBuilder::with_components`]: crate::builder::TypedBuilder::with_components
pub struct Prebuilt<T>(pub T);

impl<T: Send> ComponentBuilder for Prebuilt<T> {
    type Output = T;

    async fn build(self, _ctx: &BuilderContext<'_>) -> anyhow::Result<T> {
        Ok(self.0)
    }
}
