use chrono::NaiveDateTime;
use dyn_clone::DynClone;
use std::{
    any::{type_name, Any, TypeId},
    fmt::{self, Debug},
    hash::Hash,
};

/// Conversion to [`Any`] to workaround [#65991](https://github.com/rust-lang/rust/issues/65991).
/// Implemented for anything that's `'static` and [`Clone`].
pub trait IntoAny: DynClone + Any + Send + Sync {
    /// The conversion.
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync>;
    fn as_any(&self) -> &dyn Any;
}

dyn_clone::clone_trait_object!(IntoAny);

impl<T: 'static + Clone + Send + Sync> IntoAny for T {
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send + Sync> {
        Box::new(*self)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl Debug for dyn IntoAny + Send + Sync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Attempt to downcast to a concrete type that implements Debug
        if let Some(downcasted_value) = self.as_any().downcast_ref::<f64>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<String>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<i64>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<i32>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<u8>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<u32>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<&str>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<NaiveDateTime>() {
            return write!(f, "{:?}", downcasted_value);
        } else if let Some(downcasted_value) = self.as_any().downcast_ref::<String>() {
            return write!(f, "{:?}", downcasted_value);
        }

        write!(f, "dyn IntoAny")
    }
}

pub fn downcast<T: 'static>(value: Box<dyn IntoAny>) -> Result<T, Box<dyn IntoAny>> {
    if (*value).type_id() != TypeId::of::<T>() {
        return Err(value);
    }
    let value = value.into_any();
    // We've checked the type id.
    Ok(*Box::<dyn Any + 'static>::downcast::<T>(value).unwrap())
}

/// A [`TypeId`] and the type's name.
#[derive(Debug, Clone, Copy)]
pub struct TypeInfo {
    id: TypeId,
    name: &'static str,
}

impl TypeInfo {
    /// Gets the [`TypeId`].
    pub fn id(&self) -> TypeId {
        self.id
    }

    /// Gets the type name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the [`TypeInfo`] of the type this generic function has been
    /// instantiated with.
    pub fn of<T: 'static>() -> Self {
        TypeInfo {
            id: TypeId::of::<T>(),
            name: type_name::<T>(),
        }
    }
}

impl Hash for TypeInfo {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl PartialEq for TypeInfo {
    fn eq(&self, other: &TypeInfo) -> bool {
        self.id.eq(&other.id)
    }
}

impl Eq for TypeInfo {}

impl PartialOrd for TypeInfo {
    fn partial_cmp(&self, other: &TypeInfo) -> Option<std::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for TypeInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

/// A [`Box`]ed [`IntoAny`].

pub type DynAny = Box<dyn IntoAny + Send + Sync>;

// impl std::fmt::Debug for DynAny {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         f.debug_struct("NamedAny").finish_non_exhaustive()
//     }
// }
