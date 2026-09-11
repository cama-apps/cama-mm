# Cama concurrency trait regression checks

These compile-fail cases must remain rejected. They never execute unsound code.

## Mutable guard

```compile_fail
use std::{marker::PhantomData,sync::MutexGuard};
#[derive(Default,Eq,PartialEq,Hash)] struct NotSend(PhantomData<MutexGuard<'static,()>>);
fn send<T:Send>(_:T) {}
let m=dashmap::DashMap::new();m.insert(0,NotSend::default());send(m.get_mut(&0).unwrap());
```

## Mutable iterator item

```compile_fail
use std::{marker::PhantomData,sync::MutexGuard};
#[derive(Default,Eq,PartialEq,Hash)] struct NotSend(PhantomData<MutexGuard<'static,()>>);
fn send<T:Send>(_:T) {}
let m=dashmap::DashMap::new();m.insert(0,NotSend::default());send(m.iter_mut().next().unwrap());
```

## Owned entry

```compile_fail
use std::{marker::PhantomData,sync::MutexGuard};
#[derive(Default,Eq,PartialEq,Hash)] struct NotSend(PhantomData<MutexGuard<'static,()>>);
fn send<T:Send>(_:T) {}
let m=dashmap::DashMap::<NotSend,u32>::new();send(m.entry(NotSend::default()));
```

## Shared iterator

```compile_fail
fn send<T:Send>(_:T) {}
let m=dashmap::DashMap::new();m.insert(0,std::cell::Cell::new(0));send(m.iter());
```

## Mutable iterator

```compile_fail
fn send<T:Send>(_:T) {}
let m=dashmap::DashMap::new();m.insert(0,std::cell::Cell::new(0));send(m.iter_mut());
```

## Ordinary Send + Sync types

```
fn send<T: Send>(_: T) {}
let m=dashmap::DashMap::new();
m.insert(0,String::new());
send(m.iter());
send(m.iter_mut());
send(m.get_mut(&0).unwrap());
send(m.entry(1));
```
