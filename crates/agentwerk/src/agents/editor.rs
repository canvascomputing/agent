//! The shape every editor hook shares, so the two that exist are one
//! family in the types and not only in the naming rules.

/// A caller hook that rewrites `T` in place, reading `C` as context.
/// Concrete editors alias this rather than restating the bound, which is
/// what keeps the parameter order uniform: context first, the `&mut`
/// value last.
///
/// The value arrives holding what agentwerk would otherwise have used, so
/// an editor that writes nothing keeps the default and there is nothing
/// for it to return. One editor is held at a time; installing a second
/// replaces the first.
///
/// Aliased by [`DirectiveEditor`](super::agent::DirectiveEditor) over
/// `(str, String)` and by the reply editor behind
/// [`TicketSystem::edit_replies_on_event`](crate::TicketSystem::edit_replies_on_event)
/// over `([Event], Vec<Reply>)`.
pub(crate) type Editor<C, T> = dyn for<'a> Fn(&'a C, &mut T) + Send + Sync;
