pub mod generational;
pub mod non_generational;

// The union's active variant is controlled exclusively by Slots' occupancy bits.
// All unsafe code is confined to this module; both arenas use its safe interface.
#[allow(unsafe_code)]
mod slots;

#[cfg(test)]
mod tests;
