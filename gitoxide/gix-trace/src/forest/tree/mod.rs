//! Collected trace data: [`Span`] branches and [`Event`] leaves in a [`Tree`].
//!
//! A [`Processor`](crate::forest::Processor) receives completed trees and can inspect
//! their fields, child nodes, and durations before storing or formatting them.
use crate::forest::{Tag, Tree};
use std::{fmt, time::Duration};
use tracing::Level;

mod field;

pub use field::Field;
pub(crate) use field::FieldSet;

/// A leaf node in the log tree carrying information about a Tracing event.
#[derive(Clone, Debug)]
pub struct Event {
    /// Shared fields between events and spans.
    pub(crate) shared: Shared,

    /// The message associated with the event.
    pub(crate) message: Option<String>,

    /// The tag that the event was collected with.
    pub(crate) tag: Option<Tag>,
}

/// An internal node in the log tree carrying information about a Tracing span.
#[derive(Clone, Debug)]
pub struct Span {
    /// Shared fields between events and spans.
    pub(crate) shared: Shared,

    /// The name of the span.
    pub(crate) name: &'static str,

    /// The duration with at least one active entry, raised to at least `inner_duration`.
    pub(crate) total_duration: Duration,

    /// The sum of child spans' total durations, including overlaps between children.
    pub(crate) inner_duration: Duration,

    /// Events and spans collected while the span was open.
    pub(crate) nodes: Vec<Tree>,
}

#[derive(Clone, Debug)]
pub(crate) struct Shared {
    /// The level the event or span occurred at.
    pub(crate) level: Level,

    /// Key-value data.
    pub(crate) fields: FieldSet,
}

/// Error returned by [`Tree::event`][event].
///
/// [event]: crate::forest::Tree::event
#[derive(Debug)]
pub struct ExpectedEventError(());

impl fmt::Display for ExpectedEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Expected an event, found a span")
    }
}

impl std::error::Error for ExpectedEventError {}

/// Error returned by [`Tree::span`][span].
///
/// [span]: crate::forest::Tree::span
#[derive(Debug)]
pub struct ExpectedSpanError(());

impl fmt::Display for ExpectedSpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Expected a span, found an event")
    }
}

impl std::error::Error for ExpectedSpanError {}

impl Tree {
    /// Returns a reference to the inner [`Event`] if the tree is an event.
    ///
    /// # Errors
    ///
    /// This function returns an error if the `Tree` contains the `Span` variant.
    ///
    /// # Examples
    ///
    /// Collect a completed tree with a channel and inspect its event:
    /// ```
    /// use gix_trace::{ForestLayer, forest::{processor, tree::Event}};
    /// use tracing_subscriber::layer::SubscriberExt;
    ///
    /// let (sender, receiver) = std::sync::mpsc::channel();
    /// let processor = processor::from_fn(move |tree| {
    ///     sender.send(tree).expect("the receiver lives until the tree is inspected");
    ///     Ok(())
    /// });
    /// let subscriber = tracing_subscriber::Registry::default().with(ForestLayer::from(processor));
    /// tracing::subscriber::with_default(subscriber, || {
    ///     tracing::info!("some information");
    /// });
    /// let tree = receiver.recv()?;
    /// let event: &Event = tree.event()?;
    /// assert_eq!(event.message(), Some("some information"), "the event retains its message");
    /// # Ok::<_, Box<dyn std::error::Error>>(())
    /// ```
    pub fn event(&self) -> Result<&Event, ExpectedEventError> {
        match self {
            Tree::Event(event) => Ok(event),
            Tree::Span(_) => Err(ExpectedEventError(())),
        }
    }

    /// Returns a reference to the inner [`Span`] if the tree is a span.
    ///
    /// # Errors
    ///
    /// This function returns an error if the `Tree` contains the `Event` variant.
    ///
    /// # Examples
    ///
    /// Collect a completed tree with a channel and inspect its span:
    /// ```
    /// use gix_trace::{ForestLayer, forest::{processor, tree::Span}};
    /// use tracing_subscriber::layer::SubscriberExt;
    ///
    /// let (sender, receiver) = std::sync::mpsc::channel();
    /// let processor = processor::from_fn(move |tree| {
    ///     sender.send(tree).expect("the receiver lives until the tree is inspected");
    ///     Ok(())
    /// });
    /// let subscriber = tracing_subscriber::Registry::default().with(ForestLayer::from(processor));
    /// tracing::subscriber::with_default(subscriber, || {
    ///     tracing::info_span!("my_span").in_scope(|| tracing::info!("inside the span"));
    /// });
    /// let tree = receiver.recv()?;
    /// let span: &Span = tree.span()?;
    /// assert_eq!(span.name(), "my_span", "the completed span retains its name");
    /// # Ok::<_, Box<dyn std::error::Error>>(())
    /// ```
    pub fn span(&self) -> Result<&Span, ExpectedSpanError> {
        match self {
            Tree::Event(_) => Err(ExpectedSpanError(())),
            Tree::Span(span) => Ok(span),
        }
    }
}

impl Event {
    /// Returns the event's [`Level`].
    pub fn level(&self) -> Level {
        self.shared.level
    }

    /// Returns the event's message, if there is one.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns the event's [`Tag`], if there is one.
    pub fn tag(&self) -> Option<Tag> {
        self.tag
    }

    /// Returns the event's fields.
    pub fn fields(&self) -> &[Field] {
        &self.shared.fields
    }
}

impl Span {
    pub(crate) fn new(shared: Shared, name: &'static str) -> Self {
        Span {
            shared,
            name,
            total_duration: Duration::ZERO,
            inner_duration: Duration::ZERO,
            nodes: Vec::new(),
        }
    }

    /// Returns the span's [`Level`].
    pub fn level(&self) -> Level {
        self.shared.level
    }

    /// Returns the span's name.
    pub fn name(&self) -> &str {
        self.name
    }

    /// Returns the span's fields, including values recorded after its creation.
    ///
    /// Each key occurs once and retains its most recently recorded value.
    pub fn fields(&self) -> &[Field] {
        &self.shared.fields
    }

    /// Returns the span's child trees.
    pub fn nodes(&self) -> &[Tree] {
        &self.nodes
    }

    /// Returns the accumulated duration measured while the span had at least one active entry.
    ///
    /// Nested or concurrent entries of this same span count overlapping intervals
    /// only once: the first entry starts timing and the last matching exit stops it.
    ///
    /// When the span closes, this is raised to at least the sum of its child
    /// spans' durations. Children can run concurrently or while their parent is
    /// not entered, so this can exceed elapsed wall-clock time.
    ///
    /// For an instrumented `Future`, only time spent polling is measured.
    pub fn total_duration(&self) -> Duration {
        self.total_duration
    }

    /// Returns the sum of the child spans' total durations.
    ///
    /// Concurrent children contribute separately even when their execution overlaps.
    pub fn inner_duration(&self) -> Duration {
        self.inner_duration
    }

    /// Returns the total duration minus the sum of the child spans' durations.
    pub fn base_duration(&self) -> Duration {
        self.total_duration
            .checked_sub(self.inner_duration)
            .expect("the forest layer raises the total duration to at least the sum of its children")
    }
}
