# `src/codegen/tasks/`: async functions and their futures

## What this subtree is

This directory is the code behind `async fn`, `await`, `join`, `wait`, `cancel` and the concurrency limit. A call to an async function makes a *job*, which is also the future the call returns, and puts it on one queue. Worker threads, made as they are needed up to `max_tasks()`, take jobs off the queue and run them. One mutex and one condition variable guard the queue and every job's state, and a job that finishes wakes everyone waiting, who each look again at what they wait for.

```
async call → job on the queue → a worker runs it → its state is done or failed → await reads the value
```

Two rules keep this from deadlocking or flooding the machine with threads. *Awaiting a job that has not started runs it on the awaiting thread*, so a chain of 2,000 tasks each awaiting the next runs on one thread. *A job holds its worker while it sleeps*, so `max_tasks` limits how many tasks are in flight.

## Why it is a directory

It was `support/tasks.rs` until 2.0.13. Tasks are not support code for one kind of value, as the rest of `support/` is: they are threads, a queue and their own state, so they got a directory of their own. The name follows Jade's own word, as in `max_tasks()` and `mir/taskcheck.rs`; a future is what one call returns.

## What each file does

- *`mod.rs`* has what a job is: its layout, `{ i64 rc, i64 state, i64 cancel, ptr err, ptr next, ptr entry, ptr args, ptr free, T value }`, with the offsets other files read, its states, queued, running, done or failed, the struct its arguments are packed in, and its free function, generated per result type so the queue can release a future without knowing what it holds. It also has the lock and the condition variable, `cancelled`, and the concurrency limit.
- *`spawn.rs`* starts tasks: `new_job` makes a job and queues it, `submit` makes a worker when the queue needs one, `worker` is a worker thread's loop, `run` runs one job, and `entry` is the function generated for each async function, which unpacks the arguments, calls it, and records the value or the error. Each worker gets a 256 MiB stack, as the main thread does.
- *`wait.rs`* waits on tasks: `settle` waits for a job, running it here if it has not started, and `await`, `join`, `join_settled`, `wait` and `after` are built on it. `after` is the task behind `time.after`, which `src/lib/time.jde` starts through a hook.

## Who uses it

*Used by:* `../func/`, which lowers each MIR operation on a task to a call here, `../memory/rc.rs`, which releases a future through its free function, and `../sys/` for `time.after`.

*Depends on:* `../libcache/shared.rs` for the queue, the lock and the limit, which are state the program defines once, and `../memory/` for layouts and reference counting.

## Gotchas

*The queue's state goes through `Cx::state`.* A cached library object that starts a task must use the program's queue, not a copy of its own.

## Building and testing

```sh
cargo test codegen::
./target/debug/jade run examples/async/<name>/<name>.jde
```
