//! Command signatures: the record layout of indirect execution. Kept for
//! translating it later.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::object::{
    self, COMMAND_SIGNATURE, E_INVALIDARG, HRESULT, Iid, Object, Table, This, guid,
};
use crate::resource::PAGEABLE_IIDS;

const SIGNATURE_IIDS: [Iid; 4] = [
    guid(
        0xc36a797c,
        0xec80,
        0x4f0a,
        [0x89, 0x85, 0xa7, 0xb2, 0x47, 0x50, 0x82, 0xd1],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
pub(crate) struct Desc {
    stride: u32,
    count: u32,
    /// Each a type and up to three 32-bit fields.
    arguments: *const [u32; 4],
    nodes: u32,
}

#[repr(C)]
struct Signature {
    object: Object,
    stride: u32,
    arguments: Vec<[u32; 4]>,
}

/// Device method.
pub(crate) extern "system" fn create(
    _: This,
    desc: *const Desc,
    _root: This,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    if desc.arguments.is_null() && desc.count > 0 {
        return E_INVALIDARG;
    }
    let arguments = if desc.count == 0 {
        Vec::new()
    } else {
        // SAFETY: the title passes `count` argument descriptions.
        unsafe { std::slice::from_raw_parts(desc.arguments, desc.count as usize) }.to_vec()
    };
    tracing::debug!(stride = desc.stride, ?arguments, "command signature");
    let signature = Box::new(Signature {
        object: Object::new(&FUNCTIONS),
        stride: desc.stride,
        arguments,
    });
    object::hand_out(COMMAND_SIGNATURE, signature, iid, &SIGNATURE_IIDS, out)
}

/// A signature's record stride and arguments (type, then up to three
/// fields each).
pub(crate) fn layout<'a>(this: This) -> Option<(u32, &'a [[u32; 4]])> {
    if this.is_null() {
        return None;
    }
    // SAFETY: the title passes one of our signatures.
    let s = unsafe { &*this.cast::<Signature>() };
    Some((s.stride, &s.arguments))
}


/// The console's own indirect argument types (`D3D12XBOX_INDIRECT_ARGUMENT_TYPE_*`)
/// by number, for the logs. The title's signatures seen so far hold one
/// argument of type 0, 1, 2, 10 or 105 each; the others are not run.
pub(crate) fn console_kind(kind: u32) -> Option<&'static str> {
    Some(match kind {
        100 => "padding",
        101 => "primitive topology",
        102 => "descriptor table",
        103 => "pipeline state",
        104 => "dispatch with flags (DispatchX)",
        105 => "single-pass dispatch (DispatchL2)",
        106 => "binning batch break",
        107 => "mesh dispatch (DispatchMeshX)",
        108 => "pipeline flush",
        109 => "compute launch parameters",
        _ => return None,
    })
}

/// Which of a signature's arguments the host cannot run, named, for the log
/// of a dropped execution (empty if every type is one it runs on its own).
pub(crate) fn not_run(arguments: &[[u32; 4]]) -> String {
    let mut named = Vec::new();
    for [kind, ..] in arguments {
        if let Some(name) = console_kind(*kind)
            && *kind != 105
        {
            named.push(format!("{kind} {name}"));
        }
    }
    if arguments.len() > 1 {
        named.push(format!("{} arguments in one record", arguments.len()));
    }
    named.join(", ")
}

/// The dispatch flags of a single-pass dispatch (type 105) that the host
/// cannot honour: counts in threads (`USE_THREAD_DIMENSIONS`, 0x20) would
/// need the program's group size to divide by, which the host's indirect
/// dispatch does not have.
pub(crate) fn dispatch_flags_unmet(flags: u32) -> u32 {
    flags & 0x20
}

/// Whether a type-105 signature with flags the host cannot honour is
/// refused instead of run with its counts taken as groups:
/// `VICTORY_INDIRECT_FLAGS_STRICT=1`, default off (the title's signatures
/// seen so far carry no flags).
pub(crate) fn flags_strict() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_INDIRECT_FLAGS_STRICT").is_some_and(|v| v == "1"))
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    let found = object::is(iid, &SIGNATURE_IIDS).then_some(this);
    object::answer(COMMAND_SIGNATURE, iid, found, out)
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Signature>(this)
}

/// IUnknown 3, object 4, device child 1.
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<COMMAND_SIGNATURE, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{E_NOINTERFACE, E_POINTER, S_FALSE, S_OK, slot};

    #[test]
    fn null_or_argument_free_descriptions_are_rejected() {
        let null = std::ptr::null();
        let mut signature = null;
        // No description at all.
        assert_eq!(
            create(
                null,
                std::ptr::null(),
                null,
                &SIGNATURE_IIDS[0],
                &mut signature
            ),
            E_INVALIDARG
        );
        assert!(signature.is_null());
        // A count of arguments with nothing behind the pointer.
        let argumentless = Desc {
            stride: 16,
            count: 3,
            arguments: std::ptr::null(),
            nodes: 0,
        };
        assert_eq!(
            create(
                null,
                &argumentless,
                null,
                &SIGNATURE_IIDS[0],
                &mut signature
            ),
            E_INVALIDARG
        );
        assert!(signature.is_null());
    }

    #[test]
    fn signatures_without_arguments_stay_empty() {
        let null = std::ptr::null();
        // Zero arguments and no pointer is a valid, if useless, signature.
        let empty = Desc {
            stride: 16,
            count: 0,
            arguments: std::ptr::null(),
            nodes: 0,
        };
        let mut signature = null;
        assert_eq!(
            create(null, &empty, null, &SIGNATURE_IIDS[0], &mut signature),
            S_OK
        );
        let (stride, arguments) = layout(signature).expect("one of our signatures");
        assert_eq!(stride, 16);
        assert!(arguments.is_empty());
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(signature, 0x10)(signature),
            0
        );
        // The argument pointer is not read when the count is zero.
        let none: [[u32; 4]; 0] = [];
        let unread = Desc {
            stride: 16,
            count: 0,
            arguments: none.as_ptr(),
            nodes: 0,
        };
        let mut signature = null;
        assert_eq!(
            create(null, &unread, null, &SIGNATURE_IIDS[1], &mut signature),
            S_OK
        );
        let (_, arguments) = layout(signature).expect("one of our signatures");
        assert!(arguments.is_empty());
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(signature, 0x10)(signature),
            0
        );
    }

    #[test]
    fn arguments_are_copied_out_of_title_memory() {
        // A title reuses the memory its description lived in, while
        // ExecuteIndirect reads the layout long after creation.
        assert!(
            layout(std::ptr::null()).is_none(),
            "no signature, no layout"
        );
        let draw = [0u32, 3, 1, 0]; // three vertices, one instance
        let dispatch = [2u32, 4, 1, 1]; // dispatch 4x1x1 groups
        let expected = [draw, dispatch];
        let mut title_memory = expected;
        let desc = Desc {
            stride: 20,
            count: 2,
            arguments: title_memory.as_ptr(),
            nodes: 0,
        };
        let null = std::ptr::null();
        let mut signature = null;
        assert_eq!(
            create(null, &desc, null, &SIGNATURE_IIDS[0], &mut signature),
            S_OK
        );
        title_memory.fill([9, 9, 9, 9]);
        let (stride, arguments) = layout(signature).expect("one of our signatures");
        assert_eq!(stride, 20);
        assert_eq!(arguments, &expected[..]);
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(signature, 0x10)(signature),
            0
        );
    }

    #[test]
    fn signatures_answer_their_interfaces_and_count_references() {
        let arguments = [[2u32, 4, 1, 1]]; // dispatch 4x1x1 groups
        let desc = Desc {
            stride: 12,
            count: 1,
            arguments: arguments.as_ptr(),
            nodes: 0,
        };
        let null = std::ptr::null();
        let mut signature = null;
        assert_eq!(
            create(null, &desc, null, &SIGNATURE_IIDS[0], &mut signature),
            S_OK
        );
        assert!(!signature.is_null());
        let query =
            slot::<extern "system" fn(This, *const Iid, *mut This) -> HRESULT>(signature, 0x0);
        // Every interface the signature answers to hands out the same object.
        for iid in &SIGNATURE_IIDS {
            let mut out = null;
            assert_eq!(query(signature, iid, &mut out), S_OK);
            assert_eq!(out as usize, signature as usize);
        }
        // An unknown interface writes null behind and reports it missing.
        let unknown = [0u8; 16];
        let mut out = signature;
        assert_eq!(query(signature, &unknown, &mut out), E_NOINTERFACE);
        assert!(out.is_null());
        // A null interface id is tolerated rather than matched.
        let mut out = signature;
        assert_eq!(query(signature, std::ptr::null(), &mut out), E_NOINTERFACE);
        assert!(out.is_null());
        // No room for the answer is a pointer error.
        assert_eq!(query(signature, &unknown, std::ptr::null_mut()), E_POINTER);
        // References: one from creation, four from the queries, one added;
        // each release hands back what is left, the last frees the signature.
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(signature, 0x8)(signature),
            6
        );
        let release = slot::<extern "system" fn(This) -> u32>(signature, 0x10);
        for left in [5, 4, 3, 2, 1, 0] {
            assert_eq!(release(signature), left);
        }
    }

    #[test]
    fn creation_without_an_out_pointer_only_probes() {
        let arguments = [[2u32, 4, 1, 1]]; // dispatch 4x1x1 groups
        let desc = Desc {
            stride: 12,
            count: 1,
            arguments: arguments.as_ptr(),
            nodes: 0,
        };
        // A known interface with no out pointer asks whether creation would
        // succeed; the signature is dropped instead of handed out.
        assert_eq!(
            create(
                std::ptr::null(),
                &desc,
                std::ptr::null(),
                &SIGNATURE_IIDS[0],
                std::ptr::null_mut()
            ),
            S_FALSE
        );
        // A null interface id is tolerated: no interface, no signature.
        let mut signature = std::ptr::null();
        assert_eq!(
            create(
                std::ptr::null(),
                &desc,
                std::ptr::null(),
                std::ptr::null(),
                &mut signature
            ),
            E_NOINTERFACE
        );
        assert!(signature.is_null());
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn console_argument_types_are_named_and_single_pass_alone_is_run() {
        assert_eq!(not_run(&[[105, 0, 0, 0]]), "");
        assert_eq!(not_run(&[[2, 0, 0, 0]]), "");
        let named = not_run(&[[103, 0, 0, 0], [104, 0, 0, 0]]);
        assert!(named.contains("103 pipeline state") && named.contains("104 dispatch"));
        assert!(named.contains("2 arguments in one record"));
        assert_eq!(dispatch_flags_unmet(0x8008 | 0x20), 0x20);
        assert_eq!(dispatch_flags_unmet(0x8000), 0);
    }
}
