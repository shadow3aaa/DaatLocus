use proc_macro::TokenStream;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, Attribute, Data, DeriveInput, Field, Fields, GenericArgument,
    Ident, Lit, PathArguments, Token, Type, parse::Parser, parse_macro_input,
    punctuated::Punctuated,
};

#[proc_macro_attribute]
pub fn model_schema(args: TokenStream, input: TokenStream) -> TokenStream {
    let transparent = match parse_transparent_args(args) {
        Ok(transparent) => transparent,
        Err(err) => return err.to_compile_error().into(),
    };
    let input = parse_macro_input!(input as DeriveInput);
    match expand_model_schema(input, transparent) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn parse_transparent_args(args: TokenStream) -> syn::Result<bool> {
    let args = proc_macro2::TokenStream::from(args);
    if args.is_empty() {
        return Ok(false);
    }

    let parser = Punctuated::<Ident, Token![,]>::parse_terminated;
    let args = parser.parse2(args)?;
    let mut transparent = false;
    for arg in args {
        match arg.to_string().as_str() {
            "transparent" if transparent => {
                return Err(syn::Error::new_spanned(
                    arg,
                    "duplicate model_schema transparent argument",
                ));
            }
            "transparent" => transparent = true,
            _ => {
                return Err(syn::Error::new_spanned(
                    arg,
                    "unsupported model_schema argument; expected `transparent`",
                ));
            }
        }
    }
    Ok(transparent)
}

fn expand_model_schema(
    input: DeriveInput,
    transparent: bool,
) -> syn::Result<proc_macro2::TokenStream> {
    let ident = input.ident.clone();
    let schema = match &input.data {
        Data::Struct(data) if transparent => transparent_struct_schema(&input.attrs, &data.fields)?,
        Data::Struct(data) => object_struct_schema(&input.attrs, &data.fields)?,
        Data::Enum(data) => string_enum_schema(&input.attrs, &data.variants)?,
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                input,
                "model_schema does not support unions",
            ));
        }
    };

    Ok(quote! {
        #input

        impl crate::schema_utils::ModelSchema for #ident {
            fn model_schema() -> ::serde_json::Value {
                #schema
            }
        }
    })
}

fn transparent_struct_schema(
    attrs: &[Attribute],
    fields: &Fields,
) -> syn::Result<proc_macro2::TokenStream> {
    validate_serde_attrs(attrs, SerdeAttrTarget::TransparentContainer)?;
    let Fields::Unnamed(fields) = fields else {
        return Err(syn::Error::new_spanned(
            fields,
            "model_schema(transparent) requires a one-field tuple struct",
        ));
    };
    if fields.unnamed.len() != 1 {
        return Err(syn::Error::new_spanned(
            fields,
            "model_schema(transparent) requires exactly one field",
        ));
    }
    let field = fields.unnamed.first().expect("length checked");
    Ok(schema_expr_for_type(&field.ty))
}

fn object_struct_schema(
    attrs: &[Attribute],
    fields: &Fields,
) -> syn::Result<proc_macro2::TokenStream> {
    validate_serde_attrs(attrs, SerdeAttrTarget::ObjectContainer)?;
    let Fields::Named(fields) = fields else {
        return Err(syn::Error::new_spanned(
            fields,
            "model_schema only supports named-field structs unless transparent",
        ));
    };
    let rename_all = serde_rename_all(attrs)?;
    let mut properties = Vec::new();
    let mut required = Vec::new();
    for field in &fields.named {
        if field.ident.is_none() {
            return Err(syn::Error::new_spanned(field, "field must be named"));
        }
        validate_serde_attrs(&field.attrs, SerdeAttrTarget::Field)?;
        let property_name = field_property_name(field, rename_all.as_deref())?;
        let schema = schema_expr_for_type(&field.ty);
        properties.push(quote! {
            property_map.insert(#property_name.to_string(), #schema);
        });
        required.push(quote! {
            required.push(::serde_json::Value::String(#property_name.to_string()));
        });
    }

    Ok(quote! {{
        let mut property_map = ::serde_json::Map::new();
        let mut required = ::std::vec::Vec::new();
        #(#properties)*
        #(#required)*
        crate::schema_utils::model_schema(::serde_json::json!({
            "type": "object",
            "properties": property_map,
            "required": required,
            "additionalProperties": false,
        }))
    }})
}

fn string_enum_schema(
    attrs: &[Attribute],
    variants: &Punctuated<syn::Variant, syn::Token![,]>,
) -> syn::Result<proc_macro2::TokenStream> {
    validate_serde_attrs(attrs, SerdeAttrTarget::EnumContainer)?;
    let rename_all = serde_rename_all(attrs)?;
    let mut values = Vec::new();
    for variant in variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "model_schema only supports unit variants for enums",
            ));
        }
        validate_serde_attrs(&variant.attrs, SerdeAttrTarget::Variant)?;
        let name = serde_rename(&variant.attrs)?
            .unwrap_or_else(|| apply_rename_all(&variant.ident.to_string(), rename_all.as_deref()));
        values.push(name);
    }

    Ok(quote! {
        crate::schema_utils::string_enum_schema(&[#(#values),*])
    })
}

fn field_property_name(field: &Field, rename_all: Option<&str>) -> syn::Result<String> {
    Ok(serde_rename(&field.attrs)?.unwrap_or_else(|| {
        let ident = field.ident.as_ref().expect("named field checked");
        apply_rename_all(&ident.to_string(), rename_all)
    }))
}

#[derive(Clone, Copy)]
enum SerdeAttrTarget {
    ObjectContainer,
    TransparentContainer,
    EnumContainer,
    Field,
    Variant,
}

fn validate_serde_attrs(attrs: &[Attribute], target: SerdeAttrTarget) -> syn::Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let Some(name) = meta.path.get_ident().map(Ident::to_string) else {
                return Err(meta.error("model_schema only supports simple serde attribute names"));
            };

            if name == "rename_all" {
                let value = parse_lit_str_value(&meta)?;
                if !is_supported_rename_all(&value) {
                    return Err(meta.error(format!(
                        "unsupported serde rename_all value `{value}` for model_schema"
                    )));
                }
                if !serde_attr_allowed(&name, target) {
                    return Err(meta.error(format!(
                        "serde attribute `{name}` is not supported here by model_schema"
                    )));
                }
                return Ok(());
            }

            if !serde_attr_allowed(&name, target) {
                return Err(meta.error(format!(
                    "serde attribute `{name}` is not supported by model_schema; use a narrow schema-only mirror type when serde behavior differs from the model contract"
                )));
            }

            consume_nested_meta_value(&meta)?;
            Ok(())
        })?;
    }
    Ok(())
}

fn serde_attr_allowed(name: &str, target: SerdeAttrTarget) -> bool {
    match target {
        SerdeAttrTarget::ObjectContainer => matches!(name, "deny_unknown_fields" | "rename_all"),
        SerdeAttrTarget::TransparentContainer => matches!(name, "transparent"),
        SerdeAttrTarget::EnumContainer => matches!(name, "rename_all"),
        SerdeAttrTarget::Field => matches!(name, "rename" | "alias" | "default"),
        SerdeAttrTarget::Variant => matches!(name, "rename" | "alias"),
    }
}

fn is_supported_rename_all(value: &str) -> bool {
    matches!(
        value,
        "snake_case" | "lowercase" | "UPPERCASE" | "kebab-case" | "camelCase"
    )
}

fn schema_expr_for_type(ty: &Type) -> proc_macro2::TokenStream {
    match ty {
        Type::Reference(reference) => schema_expr_for_type(&reference.elem),
        Type::Path(path) => {
            let Some(segment) = path.path.segments.last() else {
                return compile_error("unsupported empty type path");
            };
            let ident = segment.ident.to_string();
            match ident.as_str() {
                "String" | "str" => quote! { crate::schema_utils::string_schema() },
                "bool" => quote! { crate::schema_utils::boolean_schema() },
                "usize" | "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" | "isize" => {
                    quote! { crate::schema_utils::integer_schema() }
                }
                "f32" | "f64" => quote! { crate::schema_utils::number_schema() },
                "Option" => match first_generic_type(&segment.arguments) {
                    Ok(inner) => {
                        let inner_schema = schema_expr_for_type(inner);
                        quote! { crate::schema_utils::nullable_schema(#inner_schema) }
                    }
                    Err(err) => err.to_compile_error(),
                },
                "Vec" => match first_generic_type(&segment.arguments) {
                    Ok(inner) => {
                        let inner_schema = schema_expr_for_type(inner);
                        quote! { crate::schema_utils::array_schema(#inner_schema) }
                    }
                    Err(err) => err.to_compile_error(),
                },
                _ => quote! {
                    <#ty as crate::schema_utils::ModelSchema>::model_schema()
                },
            }
        }
        _ => compile_error(
            "model_schema only supports scalar, Option<T>, Vec<T>, and ModelSchema types",
        ),
    }
}

fn first_generic_type(arguments: &PathArguments) -> syn::Result<&Type> {
    let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) = arguments
    else {
        return Err(syn::Error::new_spanned(
            arguments,
            "generic type argument is required",
        ));
    };
    let Some(GenericArgument::Type(ty)) = args.first() else {
        return Err(syn::Error::new_spanned(
            arguments,
            "first generic argument must be a type",
        ));
    };
    Ok(ty)
}

fn serde_rename(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut rename = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value: Lit = meta.value()?.parse()?;
                if let Lit::Str(value) = value {
                    rename = Some(value.value());
                }
            } else {
                consume_nested_meta_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(rename)
}

fn serde_rename_all(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut rename_all = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value: Lit = meta.value()?.parse()?;
                if let Lit::Str(value) = value {
                    rename_all = Some(value.value());
                }
            } else {
                consume_nested_meta_value(&meta)?;
            }
            Ok(())
        })?;
    }
    Ok(rename_all)
}

fn consume_nested_meta_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        let _: Lit = meta.value()?.parse()?;
    }
    Ok(())
}

fn parse_lit_str_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<String> {
    let value: Lit = meta.value()?.parse()?;
    if let Lit::Str(value) = value {
        Ok(value.value())
    } else {
        Err(meta.error("expected string literal"))
    }
}

fn apply_rename_all(name: &str, rename_all: Option<&str>) -> String {
    match rename_all {
        Some("snake_case") => to_snake_case(name),
        Some("lowercase") => name.to_ascii_lowercase(),
        Some("UPPERCASE") => name.to_ascii_uppercase(),
        Some("kebab-case") => to_snake_case(name).replace('_', "-"),
        Some("camelCase") => {
            let snake = to_snake_case(name);
            let mut parts = snake.split('_');
            let mut output = parts.next().unwrap_or_default().to_string();
            for part in parts {
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    output.push(first.to_ascii_uppercase());
                    output.push_str(chars.as_str());
                }
            }
            output
        }
        _ => name.to_string(),
    }
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    let mut previous_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if previous_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            previous_lower_or_digit = false;
        } else {
            previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            out.push(ch);
        }
    }
    out
}

fn compile_error(message: &str) -> proc_macro2::TokenStream {
    let message = syn::LitStr::new(message, proc_macro2::Span::call_site());
    quote! { compile_error!(#message) }
}
