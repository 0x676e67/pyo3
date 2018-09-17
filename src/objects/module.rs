// Copyright (c) 2017-present PyO3 Project and Contributors
//
// based on Daniel Grunwald's https://github.com/dgrunwald/rust-cpython

use conversion::{IntoPyTuple, ToPyObject};
use err::{PyErr, PyResult};
use ffi;
use init_once;
use instance::PyObjectWithToken;
use object::PyObject;
use objectprotocol::ObjectProtocol;
use objects::{exc, PyDict, PyObjectRef, PyType};
use python::{Python, ToPyPointer};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::str;
use typeob::{initialize_type, PyTypeInfo};
use GILPool;

/// Represents a Python `module` object.
#[repr(transparent)]
pub struct PyModule(PyObject);

pyobject_native_type!(PyModule, ffi::PyModule_Type, ffi::PyModule_Check);

impl PyModule {
    /// Create a new module object with the `__name__` attribute set to name.
    pub fn new<'p>(py: Python<'p>, name: &str) -> PyResult<&'p PyModule> {
        let name = CString::new(name)?;
        unsafe { py.from_owned_ptr_or_err(ffi::PyModule_New(name.as_ptr())) }
    }

    /// Import the Python module with the specified name.
    pub fn import<'p>(py: Python<'p>, name: &str) -> PyResult<&'p PyModule> {
        let name = CString::new(name)?;
        unsafe { py.from_owned_ptr_or_err(ffi::PyImport_ImportModule(name.as_ptr())) }
    }

    /// Loads the python code specified into a new module
    /// 'code' is the raw Python you want to load into the module
    /// 'file_name' is the file name to associate with the module
    ///     (this is used when Python reports errors, for example)
    /// 'module_name' is the name to give the module
    #[cfg(Py_3)]
    pub fn from_code<'p>(
        py: Python<'p>,
        code: &str,
        file_name: &str,
        module_name: &str,
    ) -> PyResult<&'p PyModule> {
        let data = CString::new(code)?;
        let filename = CString::new(file_name)?.as_ptr();
        let module = CString::new(module_name)?;

        unsafe {
            let cptr = ffi::Py_CompileString(data.as_ptr(), filename, ffi::Py_file_input);
            if cptr.is_null() {
                return Err(PyErr::fetch(py));
            }

            let mptr = ffi::PyImport_ExecCodeModuleEx(module.as_ptr(), cptr, filename);
            if mptr.is_null() {
                return Err(PyErr::fetch(py));
            }

            <&PyModule as ::conversion::FromPyObject>::extract(py.from_owned_ptr_or_err(mptr)?)
        }
    }

    /// Return the dictionary object that implements module's namespace;
    /// this object is the same as the `__dict__` attribute of the module object.
    pub fn dict(&self) -> &PyDict {
        unsafe {
            self.py()
                .from_owned_ptr::<PyDict>(ffi::PyModule_GetDict(self.as_ptr()))
        }
    }

    unsafe fn str_from_ptr(&self, ptr: *const c_char) -> PyResult<&str> {
        if ptr.is_null() {
            Err(PyErr::fetch(self.py()))
        } else {
            let slice = CStr::from_ptr(ptr).to_bytes();
            match str::from_utf8(slice) {
                Ok(s) => Ok(s),
                Err(e) => Err(PyErr::from_instance(exc::UnicodeDecodeError::new_utf8(
                    self.py(),
                    slice,
                    e,
                )?)),
            }
        }
    }

    /// Gets the module name.
    ///
    /// May fail if the module does not have a `__name__` attribute.
    pub fn name(&self) -> PyResult<&str> {
        unsafe { self.str_from_ptr(ffi::PyModule_GetName(self.as_ptr())) }
    }

    /// Gets the module filename.
    ///
    /// May fail if the module does not have a `__file__` attribute.
    pub fn filename(&self) -> PyResult<&str> {
        unsafe { self.str_from_ptr(ffi::PyModule_GetFilename(self.as_ptr())) }
    }

    /// Calls a function in the module.
    /// This is equivalent to the Python expression: `getattr(module, name)(*args, **kwargs)`
    pub fn call<A>(&self, name: &str, args: A, kwargs: Option<PyDict>) -> PyResult<&PyObjectRef>
    where
        A: IntoPyTuple,
    {
        self.getattr(name)?.call(args, kwargs)
    }

    /// Calls a function in the module.
    /// This is equivalent to the Python expression: `getattr(module, name)()`
    pub fn call0(&self, name: &str) -> PyResult<&PyObjectRef> {
        self.getattr(name)?.call0()
    }

    /// Calls a function in the module.
    /// This is equivalent to the Python expression: `getattr(module, name)(*args)`
    pub fn call1<A>(&self, name: &str, args: A) -> PyResult<&PyObjectRef>
    where
        A: IntoPyTuple,
    {
        self.getattr(name)?.call1(args)
    }

    /// Gets a member from the module.
    /// This is equivalent to the Python expression: `getattr(module, name)`
    pub fn get(&self, name: &str) -> PyResult<&PyObjectRef> {
        self.getattr(name)
    }

    /// Adds a member to the module.
    ///
    /// This is a convenience function which can be used from the module's initialization function.
    pub fn add<V>(&self, name: &str, value: V) -> PyResult<()>
    where
        V: ToPyObject,
    {
        self.setattr(name, value)
    }

    /// Adds a new extension type to the module.
    ///
    /// This is a convenience function that initializes the `class`,
    /// sets `new_type.__module__` to this module's name,
    /// and adds the type to this module.
    pub fn add_class<T>(&self) -> PyResult<()>
    where
        T: PyTypeInfo,
    {
        let ty = unsafe {
            let ty = <T as PyTypeInfo>::type_object();

            if ((*ty).tp_flags & ffi::Py_TPFLAGS_READY) != 0 {
                PyType::new::<T>()
            } else {
                // automatically initialize the class
                initialize_type::<T>(self.py(), Some(self.name()?)).unwrap_or_else(|_| {
                    panic!("An error occurred while initializing class {}", T::NAME)
                });
                PyType::new::<T>()
            }
        };

        self.setattr(T::NAME, ty)
    }

    /// Adds a function to a module, using the functions __name__ as name.
    ///
    /// Use this together with the`#[pyfunction]` and [wrap_function!] macro.
    ///
    /// ```rust,ignore
    /// m.add_function(wrap_function!(double));
    /// ```
    ///
    /// You can also add a function with a custom name using [add](PyModule::add):
    ///
    /// ```rust,ignore
    /// m.add("also_double", wrap_function!(double)(py));
    /// ```
    pub fn add_function(&self, wrapper: &Fn(Python) -> PyObject) -> PyResult<()> {
        let function = wrapper(self.py());
        let name = function
            .getattr(self.py(), "__name__")
            .expect("A function must have a __name__");
        self.add(name.extract(self.py()).unwrap(), function)
    }
}

#[cfg(Py_3)]
#[doc(hidden)]
/// Builds a module (or null) from a user given initializer. Used for `#[pymodinit]`.
pub unsafe fn make_module(
    name: &str,
    doc: &str,
    initializer: impl Fn(Python, &PyModule) -> PyResult<()>,
) -> *mut ffi::PyObject {
    use python::IntoPyPointer;

    init_once();

    #[cfg(py_sys_config = "WITH_THREAD")]
    // > Changed in version 3.7: This function is now called by Py_Initialize(), so you don’t have
    // > to call it yourself anymore.
    #[cfg(not(Py_3_7))]
    ffi::PyEval_InitThreads();

    static mut MODULE_DEF: ffi::PyModuleDef = ffi::PyModuleDef_INIT;
    // We can't convert &'static str to *const c_char within a static initializer,
    // so we'll do it here in the module initialization:
    MODULE_DEF.m_name = name.as_ptr() as *const _;

    let module = ffi::PyModule_Create(&mut MODULE_DEF);
    if module.is_null() {
        return module;
    }

    let _pool = GILPool::new();
    let py = Python::assume_gil_acquired();
    let module = match py.from_owned_ptr_or_err::<PyModule>(module) {
        Ok(m) => m,
        Err(e) => {
            e.restore(py);
            return ptr::null_mut();
        }
    };

    module
        .add("__doc__", doc)
        .expect("Failed to add doc for module");
    match initializer(py, module) {
        Ok(_) => module.into_ptr(),
        Err(e) => {
            e.restore(py);
            ptr::null_mut()
        }
    }
}

#[cfg(not(Py_3))]
#[doc(hidden)]
/// Builds a module (or null) from a user given initializer. Used for `#[pymodinit]`.
pub unsafe fn make_module(
    name: &str,
    doc: &str,
    initializer: impl Fn(Python, &PyModule) -> PyResult<()>,
) {
    init_once();

    #[cfg(py_sys_config = "WITH_THREAD")]
    ffi::PyEval_InitThreads();

    let _name = name.as_ptr() as *const _;
    let _pool = GILPool::new();
    let py = Python::assume_gil_acquired();
    let _module = ffi::Py_InitModule(_name, ptr::null_mut());
    if _module.is_null() {
        return;
    }

    let _module = match py.from_borrowed_ptr_or_err::<PyModule>(_module) {
        Ok(m) => m,
        Err(e) => {
            e.restore(py);
            return;
        }
    };

    _module
        .add("__doc__", doc)
        .expect("Failed to add doc for module");
    if let Err(e) = initializer(py, _module) {
        e.restore(py)
    }
}
