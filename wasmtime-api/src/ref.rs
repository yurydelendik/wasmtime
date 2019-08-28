use std::cell::{self, RefCell};
use std::rc::Rc;

#[derive(Debug)]
pub struct Ref<T>(Rc<RefCell<T>>);

impl<T> Ref<T> {
    pub fn new(item: T) -> Ref<T> {
        Ref(Rc::new(RefCell::new(item)))
    }

    pub fn borrow(&self) -> cell::Ref<T> {
        self.0.borrow()
    }

    pub fn borrow_mut(&self) -> cell::RefMut<T> {
        self.0.borrow_mut()
    }

    pub fn ptr_eq(&self, other: &Ref<T>) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Ref<T> {
        Ref(self.0.clone())
    }
}
