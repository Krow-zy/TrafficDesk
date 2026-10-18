//! Windows' own DNS client cache, read locally.
//!
//! `DnsQuery_W` with `DNS_QUERY_NO_WIRE_QUERY` looks like the obvious source of
//! cached names, but on current Windows it only answers from the hosts file: a
//! name that `Get-DnsClientCache` lists with live A records still comes back as
//! `DNS_ERROR_RCODE_NAME_ERROR` (measured on 10.0.26300, also right after a
//! fresh resolution), and `DNS_CACHE_ENTRY` carries no record data at all
//! (`wDataLength` is 0 for every entry). The records are reachable only through
//! the CIM class `MSFT_DNSClientCache`, which is what `Get-DnsClientCache`
//! reads.
//!
//! Querying it is local: no packet is sent and no cache entry is created, so
//! watching traffic still cannot create traffic or pollute the cache.

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::{VARIANT, VT_BSTR, VT_I2, VT_I4, VT_UI2, VariantClear};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemContext, IWbemLocator, IWbemServices,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
};
use windows::core::{BSTR, GUID, PCWSTR, w};

/// `CLSID_WbemLocator`; the generated bindings do not export it.
const CLSID_WBEM_LOCATOR: GUID = GUID::from_u128(0x4590f811_1d3a_11d0_891f_00aa004b2e24);
/// `RPC_C_AUTHN_WINNT` and `RPC_C_AUTHZ_NONE`, which live in the RPC bindings
/// that this build does not pull in.
const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;
const NAMESPACE: &str = "ROOT\\StandardCimv2";
const QUERY: &str = "SELECT Name, Type, Data FROM MSFT_DNSClientCache";
/// Bound on a runaway cache; a busy machine holds a few hundred records.
const MAX_ROWS: usize = 4096;
/// Per-batch wait, so a wedged provider returns instead of hanging the poller.
const BATCH_TIMEOUT_MS: i32 = 2_000;
/// Whole-query budget. A stalled provider must not hold up the poll loop.
const TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// One cache record: the queried name, its record type and its data.
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub data: String,
}

/// Every record in the local DNS client cache. A missing or empty cache is a
/// success with no records; only a real WMI failure reports an error.
pub fn records() -> Result<Vec<Record>, String> {
    unsafe { collect().map_err(|e| format!("{e:?}")) }
}

unsafe fn collect() -> windows::core::Result<Vec<Record>> {
    // The polling thread needs a COM apartment. S_FALSE ("already initialized")
    // and RPC_E_CHANGED_MODE are both fine here.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

    let locator: IWbemLocator = unsafe {
        CoCreateInstance(
            &CLSID_WBEM_LOCATOR,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )?
    };
    let empty = BSTR::new();
    let services: IWbemServices = unsafe {
        locator.ConnectServer(
            &BSTR::from(NAMESPACE),
            &empty,
            &empty,
            &empty,
            0,
            &empty,
            None::<&IWbemContext>,
        )?
    };
    // The proxy has to be told to impersonate the caller before it will run a
    // query; this is the step the WMI samples do for exactly this error.
    let _ = unsafe {
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    };
    let enumerator: IEnumWbemClassObject = unsafe {
        services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(QUERY),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None::<&IWbemContext>,
        )?
    };

    let mut out: Vec<Record> = Vec::with_capacity(64);
    let started = std::time::Instant::now();
    while out.len() < MAX_ROWS && started.elapsed() < TOTAL_BUDGET {
        let mut batch = [const { None }; 16];
        let mut returned = 0u32;
        let status = unsafe { enumerator.Next(BATCH_TIMEOUT_MS, &mut batch, &mut returned) };
        if status.is_err() {
            return Err(status.into());
        }
        if returned == 0 {
            break;
        }
        for object in batch.iter().take(returned as usize).flatten() {
            out.push(Record {
                name: unsafe { string_property(object, w!("Name")) }.unwrap_or_default(),
                rtype: unsafe { number_property(object, w!("Type")) }.unwrap_or(0),
                data: unsafe { string_property(object, w!("Data")) }.unwrap_or_default(),
            });
        }
    }
    Ok(out)
}

unsafe fn string_property(object: &IWbemClassObject, name: PCWSTR) -> Option<String> {
    let mut value = VARIANT::default();
    unsafe { object.Get(name, 0, &mut value, None, None) }.ok()?;
    let text = unsafe { borrow_string(&value) };
    let _ = unsafe { VariantClear(&mut value) };
    text
}

unsafe fn number_property(object: &IWbemClassObject, name: PCWSTR) -> Option<u16> {
    let mut value = VARIANT::default();
    unsafe { object.Get(name, 0, &mut value, None, None) }.ok()?;
    let number = unsafe { borrow_number(&value) };
    let _ = unsafe { VariantClear(&mut value) };
    number
}

unsafe fn borrow_string(value: &VARIANT) -> Option<String> {
    let inner = unsafe { &*value.Anonymous.Anonymous };
    if inner.vt == VT_BSTR {
        return Some(unsafe { inner.Anonymous.bstrVal.to_string() });
    }
    None
}

/// This provider returns CIM `uint16` as `VT_I4`, but accept the narrow forms
/// too so a provider change cannot silently drop the record type.
unsafe fn borrow_number(value: &VARIANT) -> Option<u16> {
    let inner = unsafe { &*value.Anonymous.Anonymous };
    let raw = &inner.Anonymous;
    if inner.vt == VT_I4 {
        return u16::try_from(unsafe { raw.lVal }).ok();
    }
    if inner.vt == VT_UI2 {
        return Some(unsafe { raw.uiVal });
    }
    if inner.vt == VT_I2 {
        return u16::try_from(unsafe { raw.iVal }).ok();
    }
    None
}
