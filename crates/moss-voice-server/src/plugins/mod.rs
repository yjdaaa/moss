//! NV11 opt-in information plugins. Providers are deliberately disabled until individually wired and verified.
pub mod cache;
pub mod http;
pub mod lunar;
pub mod metaso;
pub mod news;
pub mod newsnow;
pub mod rag;
pub mod runtime;
pub mod search;
pub mod weather;

#[cfg(test)]
mod contract_tests;
