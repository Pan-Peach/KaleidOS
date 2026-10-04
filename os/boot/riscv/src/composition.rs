//! Boot loads one profile-selected artifact; the component owns the graph.

pub(crate) fn start() {
    let name = option_env!("CONFIG_BOOT_COMPONENT").unwrap_or("");
    if name.is_empty() {
        return;
    }
    match kernel::component::load::load_and_start(
        name.as_bytes(),
        kernel::component::endpoint::ExecutionDomain::KernelNative,
    ) {
        Ok(id) => kernel::log!("boot", "{}: ready (id={})", name, id.raw()),
        Err(error) => kernel::log!("boot", "{}: {:?}; monitor fallback", name, error),
    }
}
