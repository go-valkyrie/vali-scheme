//! Mapping one tree of lists and vectors to another without recursing.
//!
//! Datums and syntax objects nest as deeply as the program does, and a
//! host cannot catch a stack overflow, so a pass over them keeps its
//! pending work on a stack of its own (ADR 0014 #2). [`map_tree`] is that
//! stack, for the passes that turn each node into a node of another tree.

/// One node of the source tree, taken apart one level.
#[derive(Debug)]
pub enum Split<'a, S, T> {
    /// A node with no parts to map, already mapped.
    Leaf(T),
    /// A list: its elements, and its dotted tail if it has one.
    List {
        items: Vec<&'a S>,
        tail: Option<&'a S>,
    },
    /// A vector's elements.
    Vector(Vec<&'a S>),
}

/// How one tree maps to another.
pub trait TreeMap<'a> {
    /// The source tree's node.
    type Src: 'a;
    /// The new tree's node.
    type Out;
    /// What can go wrong.
    type Error;

    /// Takes `node` apart one level.
    fn split(
        &mut self,
        node: &'a Self::Src,
    ) -> Result<Split<'a, Self::Src, Self::Out>, Self::Error>;

    /// The new node for the list `node`, from its mapped parts.
    fn list(
        &mut self,
        node: &'a Self::Src,
        items: Vec<Self::Out>,
        tail: Option<Self::Out>,
    ) -> Result<Self::Out, Self::Error>;

    /// The new node for the vector `node`, from its mapped elements.
    fn vector(
        &mut self,
        node: &'a Self::Src,
        items: Vec<Self::Out>,
    ) -> Result<Self::Out, Self::Error>;
}

/// A node being mapped, with the parts mapped so far.
struct Frame<'a, S, T> {
    node: &'a S,
    parts: std::vec::IntoIter<&'a S>,
    tail: Option<&'a S>,
    /// Whether the tail is the part being mapped.
    in_tail: bool,
    vector: bool,
    done: Vec<T>,
    done_tail: Option<T>,
}

/// Maps the tree under `root` with `map`, parts before the nodes that hold
/// them, on a stack of its own.
pub fn map_tree<'a, M: TreeMap<'a>>(map: &mut M, root: &'a M::Src) -> Result<M::Out, M::Error> {
    let mut stack: Vec<Frame<'a, M::Src, M::Out>> = Vec::new();
    // A node mapped, waiting to be given to the one that holds it.
    let mut finished = None;
    let mut enter = Some(root);
    loop {
        if let Some(node) = enter.take() {
            match map.split(node)? {
                Split::Leaf(out) => finished = Some(out),
                Split::List { items, tail } => stack.push(Frame {
                    node,
                    done: Vec::with_capacity(items.len()),
                    parts: items.into_iter(),
                    tail,
                    in_tail: false,
                    vector: false,
                    done_tail: None,
                }),
                Split::Vector(items) => stack.push(Frame {
                    node,
                    done: Vec::with_capacity(items.len()),
                    parts: items.into_iter(),
                    tail: None,
                    in_tail: false,
                    vector: true,
                    done_tail: None,
                }),
            }
        }
        let Some(top) = stack.last_mut() else {
            return Ok(finished.expect("the root mapped"));
        };
        if let Some(out) = finished.take() {
            if top.in_tail {
                top.done_tail = Some(out);
            } else {
                top.done.push(out);
            }
        }
        if let Some(part) = top.parts.next() {
            enter = Some(part);
        } else if let Some(tail) = top.tail.take() {
            top.in_tail = true;
            enter = Some(tail);
        } else {
            let frame = stack.pop().expect("the top frame");
            finished = Some(if frame.vector {
                map.vector(frame.node, frame.done)?
            } else {
                map.list(frame.node, frame.done, frame.done_tail)?
            });
        }
    }
}
