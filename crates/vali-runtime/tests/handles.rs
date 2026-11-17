//! Handles (ADR 0003): Send and 'static, used through their owning task,
//! dropped from anywhere.

use std::ptr::NonNull;
use std::sync::Arc;

use vali_abi::{StrategyLayout, Tag};
use vali_runtime::{
    Allocate, Buffers, Error, Handle, Heap, HeapConfig, NoCollection, OutOfMemory, PageSource,
    References, Space, Strategy, Task, Value,
};

fn task() -> Task<NoCollection> {
    Task::new(Heap::new(&HeapConfig::default()))
}

fn list(task: &mut Task<NoCollection>, items: &[i64]) -> Handle {
    let mut list = Handle::empty_list();
    for &n in items.iter().rev() {
        list = task.cons(&Handle::fixnum(n).unwrap(), &list).unwrap();
    }
    list
}

fn to_vec(task: &mut Task<NoCollection>, mut list: Handle) -> Vec<i64> {
    let mut out = Vec::new();
    while list.is_pair() {
        out.push(list.car(task).unwrap().as_fixnum().unwrap());
        list = list.cdr(task).unwrap();
    }
    out
}

#[test]
fn lists_through_handles() {
    let mut task = task();
    let xs = list(&mut task, &[1, 2, 3]);
    assert_eq!(xs.tag(), Tag::Pair);
    assert_eq!(to_vec(&mut task, xs.clone()), [1, 2, 3]);
    xs.set_car(&mut task, &Handle::fixnum(10).unwrap()).unwrap();
    let rest = xs.cdr(&mut task).unwrap();
    rest.set_cdr(&mut task, &Handle::empty_list()).unwrap();
    assert_eq!(to_vec(&mut task, xs), [10, 2]);
}

#[test]
fn immediates_need_no_task_and_no_slot() {
    let mut task = task();
    let n = Handle::fixnum(42).unwrap();
    assert_eq!(n.as_fixnum(), Some(42));
    assert_eq!(Handle::boolean(false).as_boolean(), Some(false));
    assert_eq!(Handle::character('λ').as_character(), Some('λ'));
    assert_eq!(task.get(&n), Ok(Value::fixnum(42).unwrap()));
    assert_eq!(task.live_handles(), 0);
    // Any task can use an immediate.
    let mut other = self::task();
    assert!(other.cons(&n, &Handle::empty_list()).is_ok());
}

#[test]
fn other_values() {
    let mut task = task();
    let x = task.flonum(2.5).unwrap();
    assert_eq!(x.to_f64(&mut task), Ok(2.5));
    let s = task.string("héllo").unwrap();
    assert_eq!(s.string_value(&mut task).unwrap(), "héllo");
    let v = task.vector(3, &Handle::boolean(true)).unwrap();
    assert_eq!(v.vector_length(&mut task), Ok(3));
    v.vector_set(&mut task, 1, &s).unwrap();
    let element = v.vector_ref(&mut task, 1).unwrap();
    assert_eq!(element.string_value(&mut task).unwrap(), "héllo");
    assert_eq!(v.vector_ref(&mut task, 3).unwrap_err(), Error::OutOfRange);
}

#[test]
fn type_errors() {
    let mut task = task();
    let n = Handle::fixnum(1).unwrap();
    let s = task.string("x").unwrap();
    assert_eq!(
        n.car(&mut task).unwrap_err(),
        Error::WrongType { expected: "a pair" }
    );
    assert_eq!(
        s.vector_length(&mut task).unwrap_err(),
        Error::WrongType {
            expected: "a vector"
        }
    );
    assert_eq!(
        n.to_f64(&mut task).unwrap_err(),
        Error::WrongType {
            expected: "a flonum"
        }
    );
}

#[test]
fn another_task_cannot_use_a_handle() {
    let heap = Heap::<NoCollection>::new(&HeapConfig::default());
    let mut owner = Task::new(Arc::clone(&heap));
    let mut other = Task::new(heap);
    let xs = owner
        .cons(&Handle::boolean(true), &Handle::empty_list())
        .unwrap();
    assert_eq!(xs.car(&mut other).unwrap_err(), Error::WrongTask);
    assert_eq!(
        other.cons(&xs, &Handle::empty_list()).unwrap_err(),
        Error::WrongTask
    );
    assert!(Error::WrongTask.to_string().contains("share"));
    assert!(xs.car(&mut owner).is_ok());
}

#[test]
fn dropped_handles_are_released_at_the_next_safepoint() {
    let mut task = task();
    let s = task.string("kept").unwrap();
    let copies: Vec<Handle> = (0..10).map(|_| s.clone()).collect();
    assert_eq!(task.live_handles(), 1, "clones share one slot");
    drop(copies);
    assert_eq!(task.live_handles(), 1);
    drop(s);
    assert_eq!(task.live_handles(), 0);
    // Freed slots are reused.
    let again: Vec<Handle> = (0..300)
        .map(|i| task.string(&i.to_string()).unwrap())
        .collect();
    assert_eq!(task.live_handles(), 300);
    drop(again);
    assert_eq!(task.live_handles(), 0);
}

#[test]
fn handles_are_dropped_from_any_thread() {
    let mut task = task();
    let handles: Vec<Handle> = (0..1000)
        .map(|i| task.string(&i.to_string()).unwrap())
        .collect();
    let keep = handles[7].clone();
    std::thread::scope(|scope| {
        for chunk in handles.chunks(125) {
            let clones: Vec<Handle> = chunk.iter().flat_map(|h| [h.clone(), h.clone()]).collect();
            scope.spawn(move || drop(clones));
        }
    });
    drop(handles);
    assert_eq!(task.live_handles(), 1);
    assert_eq!(keep.string_value(&mut task).unwrap(), "7");
}

#[test]
fn handles_and_tasks_cross_await_points() {
    fn send<T: Send + 'static>(_: T) {}
    fn sync<T: Sync>() {}
    sync::<Handle>();
    let mut task = task();
    let xs = task
        .cons(&Handle::fixnum(1).unwrap(), &Handle::empty_list())
        .unwrap();
    // A future holding a task and its handles across an `.await` is `Send`,
    // so a host can `tokio::spawn` it.
    send(async move {
        std::future::ready(()).await;
        let car = xs.car(&mut task).unwrap();
        std::future::ready(()).await;
        car.as_fixnum()
    });
}

/// Counts the references the runtime holds, to check that every retain has
/// a matching release.
#[derive(Debug)]
struct Probe;

#[derive(Debug, Default)]
struct ProbeMutator {
    buffers: Buffers,
    held: i64,
}

// SAFETY: delegates allocation to `NoCollection`.
unsafe impl Allocate for Probe {
    const LAYOUT: StrategyLayout = StrategyLayout::COMPACT;
    type Heap = PageSource;
    type Mutator = ProbeMutator;

    fn new_heap(config: &HeapConfig) -> PageSource {
        NoCollection::new_heap(config)
    }

    fn new_mutator(_heap: &PageSource) -> ProbeMutator {
        ProbeMutator::default()
    }

    fn allocate(
        heap: &PageSource,
        mutator: &mut ProbeMutator,
        space: Space,
        words: usize,
    ) -> Result<NonNull<u64>, OutOfMemory> {
        NoCollection::allocate(heap, &mut mutator.buffers, space, words)
    }
}

impl References for Probe {
    fn retain(mutator: &mut ProbeMutator, v: Value) {
        assert!(v.is_pointer());
        mutator.held += 1;
    }

    fn release(mutator: &mut ProbeMutator, v: Value) {
        assert!(v.is_pointer());
        mutator.held -= 1;
    }
}

impl Strategy for Probe {
    const NAME: &'static str = "test-probe";
}

#[test]
fn every_retain_is_released() {
    let mut task = Task::<Probe>::new(Heap::new(&HeapConfig::default()));
    let held = |task: &mut Task<Probe>| {
        task.safepoint();
        format!("{:?}", task).contains("held: 0")
    };
    {
        let a = task.string("a").unwrap();
        let b = task.cons(&a, &Handle::empty_list()).unwrap();
        let _a_again = b.car(&mut task).unwrap();
        let _n = Handle::fixnum(3).unwrap();
        assert!(!held(&mut task));
        assert_eq!(task.live_handles(), 3, "a, b and a again each have a slot");
    }
    assert!(held(&mut task));
    assert_eq!(task.live_handles(), 0);
}
