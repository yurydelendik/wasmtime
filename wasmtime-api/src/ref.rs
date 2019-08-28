use std::cell::{self, RefCell};
use std::fmt;
use std::rc::Rc;

pub trait HostInfo {
    fn finalize(&mut self) {}
}

struct ContentBox<T> {
    content: T,
    host_info: cell::Cell<Option<Box<dyn HostInfo>>>,
}

pub struct Ref<T>(Rc<RefCell<ContentBox<T>>>);

impl<T> Ref<T> {
    pub fn new(item: T) -> Ref<T> {
        let content = ContentBox {
            content: item,
            host_info: cell::Cell::new(None),
        };
        Ref(Rc::new(RefCell::new(content)))
    }

    pub fn borrow(&self) -> cell::Ref<T> {
        cell::Ref::map(self.0.borrow(), |b| &b.content)
    }

    pub fn borrow_mut(&self) -> cell::RefMut<T> {
        cell::RefMut::map(self.0.borrow_mut(), |b| &mut b.content)
    }

    pub fn ptr_eq(&self, other: &Ref<T>) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn host_info(&self) -> Option<cell::RefMut<Box<dyn HostInfo>>> {
        let info = cell::RefMut::map(self.0.borrow_mut(), |b| b.host_info.get_mut());
        if info.is_none() {
            return None;
        }
        Some(cell::RefMut::map(info, |info| info.as_mut().unwrap()))
    }

    pub fn set_host_info(&self, info: Option<Box<dyn HostInfo>>) {
        self.0.borrow_mut().host_info = cell::Cell::new(info);
    }
}

impl<T> Drop for Ref<T> {
    fn drop(&mut self) {
        if let Some(info) = self.0.borrow_mut().host_info.get_mut() {
            info.finalize();
        }
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Ref<T> {
        Ref(self.0.clone())
    }
}

impl<T: fmt::Debug> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ref(")?;
        self.0.borrow().content.fmt(f)?;
        write!(f, ")")
    }
}
