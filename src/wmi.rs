//! COM/WMI access to the BIOS interface in `ROOT\WMI`: method calls on `AcerGamingFunction`
//! and `APGeAction` (`Wmi`, the app's `Backend`) and `APGeEvent` notifications (`EventStream`).
//!
//! Each call fills the method's input-parameter object with one value (`protocol::Method`
//! says which name and CIM type), runs `ExecMethod` on the class's single instance and reads
//! one output property. The connection is re-established once when the WMI service went away.

use crate::controller::{Backend, Error, Result};
use crate::protocol::Method;
use std::cell::RefCell;
use std::mem::ManuallyDrop;
use windows::core::{BSTR, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElemsize,
    SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{
    VariantClear, VARENUM, VARIANT, VARIANT_0_0_0, VT_ARRAY, VT_BSTR, VT_I2, VT_I4, VT_I8, VT_INT, VT_TYPEMASK, VT_UI1,
    VT_UI2, VT_UI4, VT_UI8, VT_UINT, VT_VARIANT,
};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_GENERIC_FLAG_TYPE, WBEM_INFINITE,
};

/// A Windows/COM error becomes `Error::Wmi`, starting with the HRESULT in hex
/// (`is_disconnect` matches on that prefix).
impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error::Wmi(format!("{:#010x} {}", e.code().0.cast_unsigned(), e.message()))
    }
}

/// Owned VARIANT, cleared on drop.
struct Var(VARIANT);

impl Var {
    fn empty() -> Var {
        Var(VARIANT::default())
    }

    fn with(vt: VARENUM, fill: impl FnOnce(&mut VARIANT_0_0_0)) -> Var {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = vt;
            fill(&mut inner.Anonymous);
        }
        Var(v)
    }

    fn i4(n: i32) -> Var {
        Var::with(VT_I4, |u| u.lVal = n)
    }

    /// A uint32 argument, which WMI passes as `VT_I4` (same bits, read back as unsigned).
    fn u32_arg(n: u64) -> Result<Var> {
        let n = u32::try_from(n).map_err(|_| Error::Range("32-bit input"))?;
        Ok(Var::i4(n.cast_signed()))
    }

    /// WMI passes 64-bit integers as decimal strings.
    fn u64_str(n: u64) -> Var {
        Var::with(VT_BSTR, |u| u.bstrVal = ManuallyDrop::new(BSTR::from(n.to_string())))
    }

    /// A uint8[] argument (SAFEARRAY of `VT_UI1`).
    fn bytes(data: &[u8]) -> Result<Var> {
        unsafe {
            let len = u32::try_from(data.len()).map_err(|_| Error::Range("byte array length"))?;
            let sa = SafeArrayCreateVector(VT_UI1, 0, len);
            if sa.is_null() {
                return Err(Error::Wmi("SafeArrayCreateVector failed".into()));
            }
            let mut p = std::ptr::null_mut();
            if let Err(e) = SafeArrayAccessData(sa, &raw mut p) {
                let _ = SafeArrayDestroy(sa);
                return Err(e.into());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast::<u8>(), data.len());
            let _ = SafeArrayUnaccessData(sa);
            Ok(Var::with(VARENUM(VT_ARRAY.0 | VT_UI1.0), |u| u.parray = sa))
        }
    }

    fn vt(&self) -> VARENUM {
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// Integer value of a uint32 (`VT_I4`) or uint64 (`VT_BSTR`) property; other integer types are
    /// accepted too.
    fn as_u64(&self) -> Option<u64> {
        unsafe {
            let u = &self.0.Anonymous.Anonymous.Anonymous;
            match self.vt() {
                VT_I4 | VT_INT => Some(u64::from(u.lVal.cast_unsigned())),
                VT_UI4 | VT_UINT => Some(u64::from(u.ulVal)),
                VT_I8 | VT_UI8 => Some(u.ullVal),
                VT_UI1 => Some(u64::from(u.bVal)),
                VT_I2 | VT_UI2 => Some(u64::from(u.uiVal)),
                VT_BSTR => u.bstrVal.to_string().trim().parse().ok(),
                _ => None,
            }
        }
    }

    /// Contents of a uint8[] property.
    fn as_bytes(&self) -> Option<Vec<u8>> {
        unsafe {
            if self.vt().0 & VT_ARRAY.0 == 0 {
                return None;
            }
            let sa = self.0.Anonymous.Anonymous.Anonymous.parray;
            if sa.is_null() || SafeArrayGetDim(sa) != 1 {
                return None;
            }
            let lo = SafeArrayGetLBound(sa, 1).ok()?;
            let hi = SafeArrayGetUBound(sa, 1).ok()?;
            let len = usize::try_from(hi - lo + 1).unwrap_or(0);
            let elem_size = SafeArrayGetElemsize(sa) as usize;
            let mut p = std::ptr::null_mut();
            SafeArrayAccessData(sa, &raw mut p).ok()?;
            // Element type without the VT_ARRAY flag.
            let elem_vt = self.vt().0 & VT_TYPEMASK.0;
            let out = match elem_size {
                1 => std::slice::from_raw_parts(p as *const u8, len).to_vec(),
                // Some providers hand back uint8[] as an array of VARIANTs.
                _ if elem_vt == VT_VARIANT.0 => std::slice::from_raw_parts(p as *const VARIANT, len)
                    .iter()
                    .map(|v| v.Anonymous.Anonymous.Anonymous.bVal)
                    .collect(),
                _ => {
                    let _ = SafeArrayUnaccessData(sa);
                    return None;
                }
            };
            let _ = SafeArrayUnaccessData(sa);
            Some(out)
        }
    }
}

impl Drop for Var {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&raw mut self.0);
        }
    }
}

/// Process-wide COM security; call once after `CoInitializeEx`.
pub fn init_security() {
    unsafe {
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );
    }
}

/// Connect to a WMI namespace as the current user, with call-level authentication and
/// impersonation on the proxy.
fn connect_ns(ns: &str) -> Result<IWbemServices> {
    unsafe {
        let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let svc =
            loc.ConnectServer(&BSTR::from(ns), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
        CoSetProxyBlanket(
            &svc,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(svc)
    }
}

/// Read one property of a WMI object.
fn get_prop(obj: &IWbemClassObject, name: &str) -> Result<Var> {
    let mut v = Var::empty();
    let name = HSTRING::from(name);
    unsafe { obj.Get(&name, 0, &raw mut v.0, None, None) }?;
    Ok(v)
}

/// HRESULTs meaning the WMI service went away (restart, sleep): reconnect and retry once.
const DISCONNECTED: [&str; 5] = ["0x800706ba", "0x800706bf", "0x80010108", "0x80010114", "0x80041015"];

fn is_disconnect(e: &Error) -> bool {
    matches!(e, Error::Wmi(m) if DISCONNECTED.iter().any(|c| m.starts_with(c)))
}

/// One WMI class with its single instance (`AcerGamingFunction`, `APGeAction`).
struct Target {
    class: IWbemClassObject,
    path: BSTR,
}

impl Target {
    /// Locate the class's instance (its `__RELPATH` is what `ExecMethod` needs) and the class
    /// definition (which holds the method signatures).
    fn find(svc: &IWbemServices, class_name: &str) -> Result<Target> {
        unsafe {
            let en = svc.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(format!("SELECT * FROM {class_name}")),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )?;
            let mut objs = [None];
            let mut got = 0u32;
            en.Next(WBEM_INFINITE, &mut objs, &raw mut got).ok()?;
            let (1, Some(inst)) = (got, objs[0].take()) else {
                return Err(Error::Wmi(format!(
                    "{class_name} not found (not an Acer gaming laptop, or not run as administrator)"
                )));
            };
            let path = get_prop(&inst, "__RELPATH")?;
            let path = match path.vt() {
                VT_BSTR => (*path.0.Anonymous.Anonymous.Anonymous.bstrVal).clone(),
                _ => return Err(Error::Wmi("instance has no path".into())),
            };
            let mut class = None;
            svc.GetObject(&BSTR::from(class_name), WBEM_FLAG_RETURN_WBEM_COMPLETE, None, Some(&raw mut class), None)?;
            let class = class.ok_or_else(|| Error::Wmi("class definition missing".into()))?;
            Ok(Target { class, path })
        }
    }
}

/// One live connection to `ROOT\WMI`.
struct Conn {
    svc: IWbemServices,
    /// `AcerGamingFunction`, located at connect time (its absence means "not supported").
    gaming: Target,
    /// `APGeAction`, located on first use (absent on some models). The outcome, failure
    /// included, is cached for the life of the connection.
    apge: RefCell<Option<std::result::Result<std::rc::Rc<Target>, Error>>>,
    /// Input-parameter signature of each method, fetched on first use.
    in_sigs: RefCell<[Option<IWbemClassObject>; Method::COUNT]>,
}

/// The firmware over WMI. Single-threaded (COM objects of the creating apartment).
pub struct Wmi {
    conn: RefCell<Conn>,
}

impl Wmi {
    /// Connect and locate `AcerGamingFunction`; fails on machines without it or without
    /// administrator rights.
    ///
    /// # Errors
    /// `Error::Wmi` when COM, the `ROOT\WMI` namespace or the `AcerGamingFunction` class can't be
    /// reached.
    pub fn connect() -> Result<Wmi> {
        Ok(Wmi { conn: RefCell::new(Conn::connect()?) })
    }

    /// Run a method; on a disconnect error, reconnect and retry once.
    fn exec(&self, m: Method, input: &Var) -> Result<Var> {
        let first = self.conn.borrow().exec(m, input);
        match first {
            Err(e) if is_disconnect(&e) => {
                let fresh = Conn::connect()?;
                *self.conn.borrow_mut() = fresh;
                self.conn.borrow().exec(m, input)
            }
            r => r,
        }
    }
}

impl Conn {
    fn connect() -> Result<Conn> {
        let svc = connect_ns("ROOT\\WMI")?;
        let gaming = Target::find(&svc, "AcerGamingFunction")?;
        Ok(Conn { svc, gaming, apge: RefCell::new(None), in_sigs: RefCell::new(Default::default()) })
    }

    /// The class/instance a method belongs to.
    fn with_target<T>(&self, m: Method, f: impl FnOnce(&Target) -> Result<T>) -> Result<T> {
        if m.class() == "AcerGamingFunction" {
            return f(&self.gaming);
        }
        let t = {
            let mut slot = self.apge.borrow_mut();
            slot.get_or_insert_with(|| Target::find(&self.svc, m.class()).map(std::rc::Rc::new)).clone()?
        };
        f(&t)
    }

    /// A fresh, empty input-parameter object for `m`.
    fn in_params(&self, m: Method) -> Result<IWbemClassObject> {
        let mut cache = self.in_sigs.borrow_mut();
        let slot = &mut cache[m.index()];
        if slot.is_none() {
            let mut sig = None;
            let name = HSTRING::from(m.name());
            self.with_target(m, |t| {
                unsafe { t.class.GetMethod(&name, 0, &raw mut sig, std::ptr::null_mut()) }.map_err(Error::from)
            })?;
            *slot = Some(sig.ok_or_else(|| Error::Wmi(format!("{} has no input signature", m.name())))?);
        }
        unsafe { slot.as_ref().unwrap().SpawnInstance(0) }.map_err(Error::from)
    }

    /// Call `m` with `input` as its single input parameter; returns its output parameter.
    fn exec(&self, m: Method, input: &Var) -> Result<Var> {
        let (in_name, out_name) = m.params();
        let params = self.in_params(m)?;
        unsafe { params.Put(&HSTRING::from(in_name), 0, &raw const input.0, 0) }?;
        let mut out = None;
        let method = BSTR::from(m.name());
        self.with_target(m, |t| {
            let flags = WBEM_GENERIC_FLAG_TYPE(0);
            unsafe { self.svc.ExecMethod(&t.path, &method, flags, None, &params, Some(&raw mut out), None) }
                .map_err(Error::from)
        })?;
        let out = out.ok_or_else(|| Error::Wmi(format!("{} returned nothing", m.name())))?;
        get_prop(&out, out_name)
    }
}

/// Error for an output parameter of an unexpected VARIANT type.
fn unexpected_output(m: Method, out: &Var) -> Error {
    Error::Wmi(format!("{}: unexpected output type {}", m.name(), out.vt().0))
}

impl Backend for Wmi {
    fn call(&self, m: Method, input: u64) -> Result<u64> {
        let v = if m.input_is_u64() { Var::u64_str(input) } else { Var::u32_arg(input)? };
        let out = self.exec(m, &v)?;
        out.as_u64().ok_or_else(|| unexpected_output(m, &out))
    }

    fn call_bytes(&self, m: Method, input: &[u8]) -> Result<u64> {
        let out = self.exec(m, &Var::bytes(input)?)?;
        out.as_u64().ok_or_else(|| unexpected_output(m, &out))
    }

    fn get_bytes(&self, m: Method, input: u64) -> Result<Vec<u8>> {
        let out = self.exec(m, &Var::u32_arg(input)?)?;
        out.as_bytes().ok_or_else(|| unexpected_output(m, &out))
    }
}

/// Blocking subscription to `APGeEvent` (hotkeys). Runs on its own MTA thread.
pub struct EventStream {
    en: IEnumWbemClassObject,
    /// Keeps the connection alive for as long as the enumerator is used.
    _svc: IWbemServices,
}

impl EventStream {
    /// Subscribe to `APGeEvent`.
    ///
    /// # Errors
    /// As `open_query`.
    pub fn open() -> Result<EventStream> {
        Self::open_query("SELECT * FROM APGeEvent")
    }

    /// Any event query in ROOT\WMI (used by the diagnostics tools).
    ///
    /// # Errors
    /// `Error::Wmi` when the namespace can't be reached or the query is rejected.
    pub fn open_query(query: &str) -> Result<EventStream> {
        let svc = connect_ns("ROOT\\WMI")?;
        let en = unsafe {
            svc.ExecNotificationQuery(
                &BSTR::from("WQL"),
                &BSTR::from(query),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
        }?;
        Ok(EventStream { en, _svc: svc })
    }

    /// Block until the next event object arrives.
    fn next_object(&self) -> Result<IWbemClassObject> {
        loop {
            let mut objs = [None];
            let mut got = 0u32;
            unsafe { self.en.Next(WBEM_INFINITE, &mut objs, &raw mut got) }.ok()?;
            if let (1, Some(o)) = (got, objs[0].take()) {
                return Ok(o);
            }
        }
    }

    /// Wait for the next event; returns its class name and, if present, a uint8[] property.
    ///
    /// # Errors
    /// `Error::Wmi` when the subscription fails, e.g. after the WMI service restarted.
    pub fn next_any(&self, bytes_prop: &str) -> Result<(String, Option<Vec<u8>>)> {
        let o = self.next_object()?;
        let class = get_prop(&o, "__CLASS")
            .ok()
            .filter(|v| v.vt() == VT_BSTR)
            .map(|v| unsafe { v.0.Anonymous.Anonymous.Anonymous.bstrVal.to_string() })
            .unwrap_or_default();
        let bytes = get_prop(&o, bytes_prop).ok().and_then(|v| v.as_bytes());
        Ok((class, bytes))
    }

    /// Wait for the next event and return its `EventDetail` bytes.
    ///
    /// # Errors
    /// `Error::Wmi` when the subscription fails or the event has no `EventDetail` bytes.
    pub fn next(&self) -> Result<Vec<u8>> {
        let o = self.next_object()?;
        get_prop(&o, "EventDetail")?.as_bytes().ok_or_else(|| Error::Wmi("EventDetail missing".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{SAFEARRAY, SAFEARRAYBOUND};
    use windows::Win32::System::Ole::{SafeArrayCreate, SafeArrayPutElement};
    use windows::Win32::System::Variant::{VT_BOOL, VT_I1, VT_NULL, VT_R8};

    #[test]
    fn disconnect_codes() {
        assert!(is_disconnect(&Error::Wmi("0x800706ba The RPC server is unavailable.".into())));
        assert!(is_disconnect(&Error::Wmi("0x80041015 Transport failure".into())));
        assert!(!is_disconnect(&Error::Wmi("0x80041003 Access denied".into())));
        assert!(!is_disconnect(&Error::Range("x")));
    }

    #[test]
    fn variant_conversions() {
        assert_eq!(Var::u64_str(0xC7AE_0000).as_u64(), Some(0xC7AE_0000));
        assert_eq!(Var::u64_str(u64::MAX).as_u64(), Some(u64::MAX));
        assert_eq!(Var::i4(-1).as_u64(), Some(0xFFFF_FFFF));
        assert_eq!(Var::empty().as_u64(), None);
        let b = Var::bytes(&[1, 2, 3, 250]).unwrap();
        assert_eq!(b.as_bytes(), Some(vec![1, 2, 3, 250]));
        assert_eq!(b.as_u64(), None);
        assert_eq!(Var::bytes(&[]).unwrap().as_bytes(), Some(vec![]));
        assert_eq!(Var::i4(5).as_bytes(), None);
    }

    fn com_error(code: u32) -> Error {
        Error::from(windows::core::Error::from_hresult(windows::core::HRESULT(code.cast_signed())))
    }

    /// A VARIANT holding the SAFEARRAY `sa` as `VT_ARRAY | elem`.
    fn array(elem: VARENUM, sa: *mut SAFEARRAY) -> Var {
        assert!(!sa.is_null());
        Var::with(VARENUM(VT_ARRAY.0 | elem.0), |u| u.parray = sa)
    }

    #[test]
    fn com_error_starts_with_lowercase_padded_hresult() {
        let Error::Wmi(m) = com_error(0x8007_06BA) else { panic!("not a WMI error") };
        assert!(m.starts_with("0x800706ba "), "{m}");
        let Error::Wmi(m) = com_error(0x8004_1003) else { panic!("not a WMI error") };
        assert!(m.starts_with("0x80041003 "), "{m}");
        let Error::Wmi(m) = com_error(1) else { panic!("not a WMI error") };
        assert!(m.starts_with("0x00000001 "), "{m}");
    }

    #[test]
    fn every_disconnect_hresult_is_detected_after_conversion() {
        for code in [0x8007_06BA, 0x8007_06BF, 0x8001_0108, 0x8001_0114, 0x8004_1015] {
            assert!(is_disconnect(&com_error(code)), "{code:#x}");
        }
        for code in [0x8007_0005, 0x8004_1003, 0x8004_1002, 0x8001_0001, 0x8007_06BB] {
            assert!(!is_disconnect(&com_error(code)), "{code:#x}");
        }
    }

    #[test]
    fn disconnect_needs_the_code_as_prefix_of_a_wmi_error() {
        assert!(is_disconnect(&Error::Wmi("0x80010108".into())));
        assert!(!is_disconnect(&Error::Wmi(String::new())));
        assert!(!is_disconnect(&Error::Wmi("0x800706b".into())));
        assert!(!is_disconnect(&Error::Wmi("call failed: 0x800706ba".into())));
        assert!(!is_disconnect(&Error::Wmi(" 0x800706ba".into())));
        assert!(!is_disconnect(&Error::Os("0x800706ba".into())));
        assert!(!is_disconnect(&Error::NotPresent("0x800706ba")));
        assert!(!is_disconnect(&Error::NeedsAc));
    }

    #[test]
    fn every_integer_variant_type_reads_as_u64() {
        assert_eq!(Var::with(VT_INT, |u| u.lVal = -2).as_u64(), Some(0xFFFF_FFFE));
        assert_eq!(Var::with(VT_UI4, |u| u.ulVal = u32::MAX).as_u64(), Some(u64::from(u32::MAX)));
        assert_eq!(Var::with(VT_UINT, |u| u.ulVal = 7).as_u64(), Some(7));
        assert_eq!(Var::with(VT_I8, |u| u.llVal = -1).as_u64(), Some(u64::MAX));
        assert_eq!(Var::with(VT_UI8, |u| u.ullVal = 0x0001_0000_0000_0100).as_u64(), Some(0x0001_0000_0000_0100));
        assert_eq!(Var::with(VT_UI1, |u| u.bVal = 0xFF).as_u64(), Some(0xFF));
        assert_eq!(Var::with(VT_I2, |u| u.iVal = -1).as_u64(), Some(0xFFFF));
        assert_eq!(Var::with(VT_UI2, |u| u.uiVal = 0x4D00).as_u64(), Some(0x4D00));
        assert_eq!(Var::i4(i32::MIN).as_u64(), Some(0x8000_0000));
        assert_eq!(Var::i4(0).as_u64(), Some(0));
    }

    #[test]
    fn non_integer_variants_read_as_none() {
        assert_eq!(Var::with(VT_NULL, |_| {}).as_u64(), None);
        assert_eq!(Var::with(VT_BOOL, |u| u.boolVal = windows::Win32::Foundation::VARIANT_BOOL(-1)).as_u64(), None);
        assert_eq!(Var::with(VT_R8, |u| u.dblVal = 42.0).as_u64(), None);
        assert_eq!(Var::with(VT_I1, |u| u.cVal = 1).as_u64(), None);
        assert_eq!(Var::with(VT_NULL, |_| {}).as_bytes(), None);
    }

    #[test]
    fn bstr_parses_trimmed_unsigned_decimal_only() {
        let bstr = |s: &str| {
            let s = BSTR::from(s);
            Var::with(VT_BSTR, |u| u.bstrVal = ManuallyDrop::new(s))
        };
        assert_eq!(bstr(" 42\r\n").as_u64(), Some(42));
        assert_eq!(bstr("18446744073709551615").as_u64(), Some(u64::MAX));
        assert_eq!(bstr("18446744073709551616").as_u64(), None);
        assert_eq!(bstr("-1").as_u64(), None);
        assert_eq!(bstr("0x10").as_u64(), None);
        assert_eq!(bstr("").as_u64(), None);
        assert_eq!(bstr("12abc").as_u64(), None);
        assert_eq!(bstr("42").as_bytes(), None);
        assert_eq!(Var::u64_str(0).as_u64(), Some(0));
        assert_eq!(Var::u64_str(0).vt(), VT_BSTR);
    }

    #[test]
    fn u32_argument_is_i4_and_rejects_wider_values() {
        let v = Var::u32_arg(u64::from(u32::MAX)).unwrap();
        assert_eq!(v.vt(), VT_I4);
        assert_eq!(v.as_u64(), Some(u64::from(u32::MAX)));
        assert_eq!(Var::u32_arg(0).unwrap().as_u64(), Some(0));
        assert_eq!(Var::u32_arg(1 << 32).err(), Some(Error::Range("32-bit input")));
        assert_eq!(Var::u32_arg(u64::MAX).err(), Some(Error::Range("32-bit input")));
    }

    #[test]
    fn large_byte_array_round_trips() {
        let data: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let v = Var::bytes(&data).unwrap();
        assert_eq!(v.vt(), VARENUM(VT_ARRAY.0 | VT_UI1.0));
        assert_eq!(v.as_bytes(), Some(data));
        let empty = Var::bytes(&[]).unwrap();
        assert_eq!(empty.vt(), VARENUM(VT_ARRAY.0 | VT_UI1.0));
        assert_eq!(empty.as_u64(), None);
    }

    #[test]
    fn byte_array_with_nonzero_lower_bound() {
        let v = unsafe {
            let sa = SafeArrayCreateVector(VT_UI1, 5, 3);
            for (idx, b) in (5i32..).zip(&[7u8, 8, 9]) {
                SafeArrayPutElement(sa, &raw const idx, std::ptr::from_ref(b).cast()).unwrap();
            }
            array(VT_UI1, sa)
        };
        assert_eq!(v.as_bytes(), Some(vec![7, 8, 9]));
    }

    #[test]
    fn array_of_variants_reads_as_bytes() {
        let v = unsafe {
            let sa = SafeArrayCreateVector(VT_VARIANT, 0, 3);
            for (idx, b) in (0i32..).zip([0u8, 0x80, 0xFF]) {
                let elem = Var::with(VT_UI1, |u| u.bVal = b);
                SafeArrayPutElement(sa, &raw const idx, (&raw const elem.0).cast()).unwrap();
            }
            array(VT_VARIANT, sa)
        };
        assert_eq!(v.as_bytes(), Some(vec![0, 0x80, 0xFF]));
        let empty = unsafe { array(VT_VARIANT, SafeArrayCreateVector(VT_VARIANT, 0, 0)) };
        assert_eq!(empty.as_bytes(), Some(vec![]));
    }

    #[test]
    fn other_arrays_are_not_bytes() {
        let ints = unsafe { array(VT_I4, SafeArrayCreateVector(VT_I4, 0, 2)) };
        assert_eq!(ints.as_bytes(), None);
        let bounds = [SAFEARRAYBOUND { cElements: 2, lLbound: 0 }; 2];
        let matrix = unsafe { array(VT_UI1, SafeArrayCreate(VT_UI1, 2, bounds.as_ptr())) };
        assert_eq!(matrix.as_bytes(), None);
        let null = Var::with(VARENUM(VT_ARRAY.0 | VT_UI1.0), |u| u.parray = std::ptr::null_mut());
        assert_eq!(null.as_bytes(), None);
    }

    #[test]
    fn unexpected_output_names_method_and_variant_type() {
        let e = unexpected_output(Method::GetGamingSysInfo, &Var::empty());
        assert_eq!(e, Error::Wmi("GetGamingSysInfo: unexpected output type 0".into()));
        let e = unexpected_output(Method::ApgeGetFunction, &Var::u64_str(1));
        assert_eq!(e, Error::Wmi("GetFunction: unexpected output type 8".into()));
        assert!(!is_disconnect(&e));
    }
}
