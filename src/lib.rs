#![feature(proc_macro_diagnostic)]
extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2;
use quote::quote;
use syn;
use syn::spanned::Spanned;

/// Turns function into partially applicable functions.
#[proc_macro_attribute]
pub fn part_app(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func_item: syn::Item = syn::parse(item).expect("failed to parse input");
    let attributes: Vec<String> = attr.to_string().split(",").map(|s| s.to_string()).collect();
    let polymorphic = attributes.contains(&"poly".to_string());
    let impl_clone = attributes.contains(&"Clone".to_string());
    if !polymorphic && impl_clone {
        func_item
            .span()
            .unstable()
            .error(r#"Cannot implement "Clone" without "poly""#)
            .emit()
    }
    if !attr.is_empty() && !polymorphic {
        func_item
            .span()
            .unstable()
            .error(r#""poly" is the only accepted attribute"#)
            .emit()
    }

    match func_item {
        syn::Item::Fn(ref func) => {
            let name = get_name(func);
            let predicate =
                syn::Ident::new(&format!("__PartialApplication__{}_", name), name.span());
            // TODO: maybe these should be public if the original function is
            // itself public
            let added_unit = concat_ident(name, "Added");
            let empty_unit = concat_ident(name, "Empty");
            let argument_vector = argument_vector(&func.sig.inputs);
            let func_out = &func.sig.output;
            let generics: Vec<_> = func.sig.generics.params.iter().map(|f| f).collect();

            // disallow where clauses
            if let Some(w) = &func.sig.generics.where_clause {
                w.span()
                    .unstable()
                    .error("part_app does not allow where clauses")
                    .emit();
            }

            let func_struct = main_struct(
                &predicate,
                &argument_vector,
                func_out,
                &generics,
                polymorphic,
            );

            let generator_func = generator_func(
                &predicate,
                name,
                &argument_vector,
                func_out,
                &empty_unit,
                &func.block,
                &generics,
                polymorphic,
            );
            let unit_structs = quote! {
                #[allow(non_camel_case_types,non_snake_case)]
                struct #added_unit;
                #[allow(non_camel_case_types,non_snake_case)]
                struct #empty_unit;
            };

            let final_call = final_call(
                &predicate,
                &argument_vector,
                &func.sig.output,
                &added_unit,
                &generics,
                polymorphic,
            );

            let argument_calls = argument_calls(
                &predicate,
                &argument_vector,
                &added_unit,
                &empty_unit,
                func_out,
                &generics,
                polymorphic,
            );

            // assemble output
            let mut out = proc_macro2::TokenStream::new();
            out.extend(unit_structs);
            out.extend(func_struct);
            out.extend(generator_func);
            out.extend(argument_calls);
            out.extend(final_call);
            // println!("{}", out);
            TokenStream::from(out)
        }
        _ => {
            func_item
                .span()
                .unstable()
                .error(
                    "Only functions can be partially applied, for structs use the builder pattern",
                )
                .emit();
            proc_macro::TokenStream::from(quote! { #func_item })
        }
    }
}

/// The portion of the signature necessary for each impl block
fn impl_signature<'a>(
    args: &Vec<&syn::PatType>,
    ret_type: &'a syn::ReturnType,
    generics: &Vec<&syn::GenericParam>,
    poly: bool,
) -> proc_macro2::TokenStream {
    let arg_names = arg_names(&args);
    let arg_types = arg_types(&args);
    let augmented_names = if !poly {
        augmented_argument_names(&arg_names)
    } else {
        Vec::new()
    };

    quote! {
        #(#generics,)*
        #(#augmented_names: Fn() -> #arg_types,)*
        BODYFN: Fn(#(#arg_types,)*) #ret_type
    }
}

/// Generates the methods used to add argument values to a partially applied function. One method is generate for each
/// argument and each method is contained in it's own impl block.
fn argument_calls<'a>(
    struct_name: &syn::Ident,
    args: &Vec<&syn::PatType>,
    unit_added: &syn::Ident,
    unit_empty: &syn::Ident,
    ret_type: &'a syn::ReturnType,
    generics: &Vec<&syn::GenericParam>,
    poly: bool,
) -> proc_macro2::TokenStream {
    let impl_sig = impl_signature(args, ret_type, generics, poly);
    let arg_name_vec = arg_names(args);
    let aug_arg_names = augmented_argument_names(&arg_name_vec);
    let arg_types = arg_types(&args);
    arg_names(args)
        .into_iter()
        .zip(&aug_arg_names)
        .zip(arg_types)
        .map(|((n, n_fn), n_type)| {
            // All variable names except the name of this function
            let free_vars: Vec<_> = arg_name_vec.iter().filter(|&x| x != &n).collect();
            let associated_vals_out: Vec<_> = arg_name_vec
                .iter()
                .map(|x| {
                    if &n == x {
                        unit_added.clone()
                    } else {
                        x.clone()
                    }
                })
                .collect();
            let val_list_out = if poly {
                quote! {#(#associated_vals_out,)*}
            } else {
                quote! {#(#associated_vals_out, #aug_arg_names,)*}
            };
            let associated_vals_in: Vec<_> = associated_vals_out
                .iter()
                .map(|x| if x == unit_added { unit_empty } else { x })
                .collect();
            let val_list_in = if poly {
                quote! {#(#associated_vals_in,)*}
            } else {
                quote! {#(#associated_vals_in, #aug_arg_names,)*}
            };
            let (transmute, self_type) = if poly {
                (quote!(transmute), quote!(self))
            } else {
                (quote!(transmute_copy), quote!(&self))
            };
            let some = if poly {
                quote! {Some(::std::sync::Arc::from(#n))}
            } else {
                quote! {Some(#n)}
            };
            let in_type = if poly {
                quote! { Box<dyn Fn() -> #n_type> }
            } else {
                quote! { #n_fn }
            };
            quote! {
                #[allow(non_camel_case_types,non_snake_case)]
                impl< #impl_sig, #(#free_vars,)* > // The impl signature
                    #struct_name<#(#generics,)* #val_list_in BODYFN> // The struct signature
                {
                    fn #n (mut self, #n: #in_type) ->
                        #struct_name<#(#generics,)* #val_list_out BODYFN>{
                        self.#n = #some;
                        unsafe {
                            ::std::mem::#transmute::<
                                #struct_name<#(#generics,)* #val_list_in BODYFN>,
                            #struct_name<#(#generics,)* #val_list_out BODYFN>,
                            >(#self_type)
                        }
                    }
                }
            }
        })
        .collect()
}

/// Generates the call function, which executes a fully filled out partially applicable struct.
fn final_call<'a>(
    struct_name: &syn::Ident,
    args: &Vec<&syn::PatType>,
    ret_type: &'a syn::ReturnType,
    unit_added: &'a syn::Ident,
    generics: &Vec<&syn::GenericParam>,
    poly: bool,
) -> proc_macro2::TokenStream {
    let impl_sig = impl_signature(args, ret_type, generics, poly);
    let arg_names = arg_names(args);
    let aug_args = augmented_argument_names(&arg_names);
    let arg_list: proc_macro2::TokenStream = if poly {
        aug_args.iter().map(|_| quote! {#unit_added,}).collect()
    } else {
        aug_args.iter().map(|a| quote! {#unit_added, #a,}).collect()
    };
    quote! {
        #[allow(non_camel_case_types,non_snake_case)]
        impl <#impl_sig> // impl signature
            // struct signature
            #struct_name<#(#generics,)* #arg_list BODYFN>
        {
            fn call(self) #ret_type { // final call
                (self.body)(#(self.#arg_names.unwrap()(),)*)
            }
        }
    }
}

/// The function called by the user to create an instance of a partially applicable function. This function always has
/// the name of the original function the macro is called on.
fn generator_func<'a>(
    struct_name: &'a syn::Ident,
    name: &'a syn::Ident,
    args: &Vec<&syn::PatType>,
    ret_type: &'a syn::ReturnType,
    empty_unit: &'a syn::Ident,
    body: &'a Box<syn::Block>,
    generics: &Vec<&syn::GenericParam>,
    poly: bool,
) -> proc_macro2::TokenStream {
    let arg_names = arg_names(&args);
    let arg_types = arg_types(&args);
    let marker_names = marker_names(&arg_names);
    let gen_types = if poly {
        quote! {#(#generics,)*}
    } else {
        quote! {#(#generics,)* #(#arg_names,)*}
    };
    let struct_types = if poly {
        arg_names.iter().map(|_| quote! {#empty_unit,}).collect()
    } else {
        quote! {#(#empty_unit,#arg_names,)*}
    };
    let body_fn = if poly {
        quote! {::std::sync::Arc::new(|#(#arg_names,)*| #body),}
    } else {
        quote! {|#(#arg_names,)*| #body,}
    };
    let where_clause = if poly {
        quote!()
    } else {
        quote! {
            where
                #(#arg_names: Fn() -> #arg_types,)*
        }
    };
    quote! {
        #[allow(non_camel_case_types,non_snake_case)]
        fn #name<#gen_types>() -> #struct_name<#(#generics,)* #struct_types
        impl Fn(#(#arg_types,)*) #ret_type>
            #where_clause
        {
            #struct_name {
                #(#arg_names: None,)*
                #(#marker_names: ::std::marker::PhantomData,)*
                body: #body_fn
            }
        }

    }
}

/// A vector of all argument names. Simple parsing.
fn arg_names<'a>(args: &Vec<&syn::PatType>) -> Vec<syn::Ident> {
    args.iter()
        .map(|f| {
            let f_pat = &f.pat;
            syn::Ident::new(&format!("{}", quote!(#f_pat)), f.span())
        })
        .collect()
}

/// The vector of names used to hold PhantomData.
fn marker_names(names: &Vec<syn::Ident>) -> Vec<syn::Ident> {
    names.iter().map(|f| concat_ident(f, "m")).collect()
}

/// Concatenates a identity with a string, returning a new identity with the same span.
fn concat_ident<'a>(ident: &'a syn::Ident, end: &str) -> syn::Ident {
    let name = format!("{}___{}", quote! {#ident}, end);
    syn::Ident::new(&name, ident.span())
}

/// Gets the name a of function.
fn get_name<'a>(func: &'a syn::ItemFn) -> &'a syn::Ident {
    // TODO: move this check somewhere else
    if let Some(r) = &func.sig.receiver() {
        r.span()
            .unstable()
            .error("Cannot make methods partially applicable yet")
            .emit();
    }
    &func.sig.ident
}

/// Filter an argument list to a pattern type
fn argument_vector<'a>(
    args: &'a syn::punctuated::Punctuated<syn::FnArg, syn::token::Comma>,
) -> Vec<&syn::PatType> {
    args.iter()
        .map(|fn_arg| match fn_arg {
            syn::FnArg::Receiver(_) => panic!("should filter out reciever arguments"),
            syn::FnArg::Typed(t) => {
                if let syn::Type::Reference(r) = t.ty.as_ref() {
                    if r.lifetime.is_none() {
                        t.span()
                            .unstable()
                            .error("part_app does not support lifetime elision")
                            .emit();
                    }
                }

                t
            }
        })
        .collect()
}

/// Retrieves the identities of an the argument list
fn arg_types<'a>(args: &Vec<&'a syn::PatType>) -> Vec<&'a syn::Type> {
    args.iter().map(|f| f.ty.as_ref()).collect()
}

/// Names to hold function types
fn augmented_argument_names<'a>(arg_names: &Vec<syn::Ident>) -> Vec<syn::Ident> {
    arg_names.iter().map(|f| concat_ident(f, "FN")).collect()
}

/// Generates the main struct for the partially applicable function.
/// All other functions are methods on this struct.
fn main_struct<'a>(
    name: &'a syn::Ident,
    args: &Vec<&syn::PatType>,
    ret_type: &'a syn::ReturnType,
    generics: &Vec<&syn::GenericParam>,
    poly: bool,
) -> proc_macro2::TokenStream {
    let arg_types = arg_types(&args);

    let arg_names = arg_names(&args);
    let arg_augmented = augmented_argument_names(&arg_names);

    let arg_list: Vec<_> = if !poly {
        arg_names
            .iter()
            .zip(arg_augmented.iter())
            .flat_map(|(n, a)| vec![n, a])
            .collect()
    } else {
        arg_names.iter().collect()
    };
    let bodyfn = if poly {
        quote! {::std::sync::Arc<BODYFN>}
    } else {
        quote! { BODYFN }
    };
    let where_clause = if poly {
        quote!(BODYFN: Fn(#(#arg_types,)*) #ret_type,)
    } else {
        quote! {
            #(#arg_augmented: Fn() -> #arg_types,)*
            BODYFN: Fn(#(#arg_types,)*) #ret_type,
        }
    };
    let names_with_m = marker_names(&arg_names);
    let option_list = if !poly {
        quote! {#(#arg_names: Option<#arg_augmented>,)*}
    } else {
        quote! {#(#arg_names: Option<::std::sync::Arc<dyn Fn() -> #arg_types>>,)*}
    };

    quote! {
        #[allow(non_camel_case_types,non_snake_case)]
        struct #name <#(#generics,)* #(#arg_list,)*BODYFN>
        where
            #where_clause
        {
            // These hold the (phantom) types which represent if a field has
            // been filled
            #(#names_with_m: ::std::marker::PhantomData<#arg_names>,)*
            // These hold the closures representing each argument
            #option_list
            // This holds the executable function
            body: #bodyfn,
        }
    }
    // TODO: Add copy here
}
