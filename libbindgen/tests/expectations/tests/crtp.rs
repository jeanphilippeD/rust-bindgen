/* automatically generated by rust-bindgen */


#![allow(non_snake_case)]


#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct Base<T> {
    pub _address: u8,
    pub _phantom_0: ::std::marker::PhantomData<T>,
}
#[repr(C)]
#[derive(Debug, Copy)]
pub struct Derived {
    pub _address: u8,
}
#[test]
fn bindgen_test_layout_Derived() {
    assert_eq!(::std::mem::size_of::<Derived>() , 1usize);
    assert_eq!(::std::mem::align_of::<Derived>() , 1usize);
}
impl Clone for Derived {
    fn clone(&self) -> Self { *self }
}
#[repr(C)]
#[derive(Debug)]
pub struct BaseWithDestructor<T> {
    pub _address: u8,
    pub _phantom_0: ::std::marker::PhantomData<T>,
}
#[repr(C)]
#[derive(Debug)]
pub struct DerivedFromBaseWithDestructor {
    pub _address: u8,
}
#[test]
fn bindgen_test_layout_DerivedFromBaseWithDestructor() {
    assert_eq!(::std::mem::size_of::<DerivedFromBaseWithDestructor>() ,
               1usize);
    assert_eq!(::std::mem::align_of::<DerivedFromBaseWithDestructor>() ,
               1usize);
}
#[test]
fn __bindgen_test_layout_template_1() {
    assert_eq!(::std::mem::size_of::<Base<Derived>>() , 1usize);
    assert_eq!(::std::mem::align_of::<Base<Derived>>() , 1usize);
}
#[test]
fn __bindgen_test_layout_template_2() {
    assert_eq!(::std::mem::size_of::<BaseWithDestructor<DerivedFromBaseWithDestructor>>()
               , 1usize);
    assert_eq!(::std::mem::align_of::<BaseWithDestructor<DerivedFromBaseWithDestructor>>()
               , 1usize);
}
