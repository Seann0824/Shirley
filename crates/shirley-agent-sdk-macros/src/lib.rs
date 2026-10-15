use proc_macro::TokenStream;

mod tool;

#[proc_macro_attribute]
pub fn tool(
    // The macro's parameters
    attr: TokenStream,
    // The annotated code block
    item: TokenStream,
) -> TokenStream {
    // Generate a new code block from the obtained original code and return it.

    tool::expand(attr, item)
}
