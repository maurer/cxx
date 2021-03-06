use crate::syntax::atom::Atom::{self, *};
use crate::syntax::{error, ident, Api, ExternFn, Ref, Struct, Ty1, Type, Types, Var};
use proc_macro2::{Delimiter, Group, Ident, TokenStream};
use quote::quote;
use syn::{Error, Result};

pub(crate) fn typecheck(apis: &[Api], types: &Types) -> Result<()> {
    let mut errors = Vec::new();

    for ty in types {
        match ty {
            Type::Ident(ident) => {
                if Atom::from(ident).is_none()
                    && !types.structs.contains_key(ident)
                    && !types.cxx.contains(ident)
                    && !types.rust.contains(ident)
                {
                    errors.push(unsupported_type(ident));
                }
            }
            Type::RustBox(ptr) => {
                if let Type::Ident(ident) = &ptr.inner {
                    if types.cxx.contains(ident) {
                        errors.push(unsupported_cxx_type_in_box(ptr));
                    }
                    if Atom::from(ident).is_none() {
                        continue;
                    }
                }
                errors.push(unsupported_box_target(ptr));
            }
            Type::UniquePtr(ptr) => {
                if let Type::Ident(ident) = &ptr.inner {
                    if types.rust.contains(ident) {
                        errors.push(unsupported_rust_type_in_unique_ptr(ptr));
                    }
                    match Atom::from(ident) {
                        None | Some(CxxString) => continue,
                        _ => {}
                    }
                }
                errors.push(unsupported_unique_ptr_target(ptr));
            }
            Type::Ref(ty) => {
                if let Type::Void(_) = ty.inner {
                    errors.push(unsupported_reference_type(ty));
                }
            }
            Type::Fn(_) => errors.push(unimplemented_fn_type(ty)),
            _ => {}
        }
    }

    for api in apis {
        match api {
            Api::Struct(strct) => {
                if strct.fields.is_empty() {
                    errors.push(struct_empty(strct));
                }
                for field in &strct.fields {
                    if is_unsized(&field.ty, types) {
                        errors.push(field_by_value(field, types));
                    }
                }
            }
            Api::CxxFunction(efn) | Api::RustFunction(efn) => {
                for arg in &efn.args {
                    if is_unsized(&arg.ty, types) {
                        errors.push(argument_by_value(arg, types));
                    }
                }
                if let Some(ty) = &efn.ret {
                    if is_unsized(ty, types) {
                        errors.push(return_by_value(ty, types));
                    }
                }
            }
            _ => {}
        }
    }

    for api in apis {
        if let Api::CxxFunction(efn) = api {
            errors.extend(check_mut_return_restriction(efn).err());
        }
        if let Api::CxxFunction(efn) | Api::RustFunction(efn) = api {
            errors.extend(check_multiple_arg_lifetimes(efn).err());
        }
    }

    ident::check_all(apis, &mut errors);

    let mut iter = errors.into_iter();
    let mut all_errors = match iter.next() {
        Some(err) => err,
        None => return Ok(()),
    };
    for err in iter {
        all_errors.combine(err);
    }
    Err(all_errors)
}

fn is_unsized(ty: &Type, types: &Types) -> bool {
    let ident = match ty {
        Type::Ident(ident) => ident,
        Type::Void(_) => return true,
        _ => return false,
    };
    ident == CxxString || types.cxx.contains(ident) || types.rust.contains(ident)
}

fn check_mut_return_restriction(efn: &ExternFn) -> Result<()> {
    match &efn.ret {
        Some(Type::Ref(ty)) if ty.mutability.is_some() => {}
        _ => return Ok(()),
    }

    for arg in &efn.args {
        if let Type::Ref(ty) = &arg.ty {
            if ty.mutability.is_some() {
                return Ok(());
            }
        }
    }

    Err(Error::new_spanned(
        efn,
        "&mut return type is not allowed unless there is a &mut argument",
    ))
}

fn check_multiple_arg_lifetimes(efn: &ExternFn) -> Result<()> {
    match &efn.ret {
        Some(Type::Ref(_)) => {}
        _ => return Ok(()),
    }

    let mut reference_args = 0;
    for arg in &efn.args {
        if let Type::Ref(_) = &arg.ty {
            reference_args += 1;
        }
    }

    if reference_args == 1 {
        Ok(())
    } else {
        Err(Error::new_spanned(
            efn,
            "functions that return a reference must take exactly one input reference",
        ))
    }
}

fn describe(ty: &Type, types: &Types) -> String {
    match ty {
        Type::Ident(ident) => {
            if types.structs.contains_key(ident) {
                "struct".to_owned()
            } else if types.cxx.contains(ident) {
                "C++ type".to_owned()
            } else if types.rust.contains(ident) {
                "opaque Rust type".to_owned()
            } else if Atom::from(ident) == Some(CxxString) {
                "C++ string".to_owned()
            } else {
                ident.to_string()
            }
        }
        Type::RustBox(_) => "Box".to_owned(),
        Type::UniquePtr(_) => "unique_ptr".to_owned(),
        Type::Ref(_) => "reference".to_owned(),
        Type::Str(_) => "&str".to_owned(),
        Type::Fn(_) => "function pointer".to_owned(),
        Type::Void(_) => "()".to_owned(),
    }
}

fn unsupported_type(ident: &Ident) -> Error {
    Error::new(ident.span(), "unsupported type")
}

fn unsupported_reference_type(ty: &Ref) -> Error {
    Error::new_spanned(ty, "unsupported reference type")
}

fn unsupported_cxx_type_in_box(unique_ptr: &Ty1) -> Error {
    Error::new_spanned(unique_ptr, error::BOX_CXX_TYPE.msg)
}

fn unsupported_box_target(unique_ptr: &Ty1) -> Error {
    Error::new_spanned(unique_ptr, "unsupported target type of Box")
}

fn unsupported_rust_type_in_unique_ptr(unique_ptr: &Ty1) -> Error {
    Error::new_spanned(unique_ptr, "unique_ptr of a Rust type is not supported yet")
}

fn unsupported_unique_ptr_target(unique_ptr: &Ty1) -> Error {
    Error::new_spanned(unique_ptr, "unsupported unique_ptr target type")
}

fn struct_empty(strct: &Struct) -> Error {
    let struct_token = strct.struct_token;
    let mut brace_token = Group::new(Delimiter::Brace, TokenStream::new());
    brace_token.set_span(strct.brace_token.span);
    let span = quote!(#struct_token #brace_token);
    Error::new_spanned(span, "structs without any fields are not supported")
}

fn field_by_value(field: &Var, types: &Types) -> Error {
    let desc = describe(&field.ty, types);
    let message = format!("using {} by value is not supported", desc);
    Error::new_spanned(field, message)
}

fn argument_by_value(arg: &Var, types: &Types) -> Error {
    let desc = describe(&arg.ty, types);
    let message = format!("passing {} by value is not supported", desc);
    Error::new_spanned(arg, message)
}

fn return_by_value(ty: &Type, types: &Types) -> Error {
    let desc = describe(ty, types);
    let message = format!("returning {} by value is not supported", desc);
    Error::new_spanned(ty, message)
}

fn unimplemented_fn_type(ty: &Type) -> Error {
    Error::new_spanned(ty, "function pointer support is not implemented yet")
}
