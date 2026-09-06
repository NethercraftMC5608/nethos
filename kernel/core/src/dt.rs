//! The flattened device tree: the only thing nk is told about the machine.
//!
//! Written against the specification rather than ported, for the same reason
//! `pkg/npkg_rpm.py` was: the format is small and stable, and a dependency
//! that has to exist before the allocator does is worse than a parser.
//!
//! Allocation-free by necessity -- this runs before there is a heap, because
//! finding out how much memory exists is what the heap is waiting for. Every
//! lookup is a fresh walk of the structure block. That is O(n) each time and
//! entirely fine: the walk is a few thousand bytes and happens a handful of
//! times during boot, once.

const MAGIC: u32 = 0xd00d_feed;

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

pub struct Fdt {
    blob: &'static [u8],
    struct_off: usize,
    struct_size: usize,
    strings_off: usize,
}

/// A node the walk is currently sitting on. Holds an offset rather than any
/// parsed content: properties are read on demand by scanning forward, so a
/// node nobody asks about costs nothing to pass over.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    fdt: &'a Fdt,
    name_off: usize,  // start of the node's name
    props_off: usize, // first token after the name
}

fn be32(b: &[u8], at: usize) -> u32 {
    // Byte at a time on purpose. The DTB is only guaranteed 8-byte aligned as
    // a whole, and with the MMU off an unaligned word load is a fault rather
    // than a slow path.
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn cstr(b: &[u8], at: usize) -> &str {
    let mut e = at;
    while e < b.len() && b[e] != 0 {
        e += 1;
    }
    // Node and property names are ASCII by specification; a device tree that
    // breaks that is corrupt in a way this parser cannot usefully report.
    core::str::from_utf8(&b[at..e]).unwrap_or("<bad utf8>")
}

/// Round up to the next 4-byte boundary. Every token and every property value
/// in the structure block is padded to one.
const fn align4(n: usize) -> usize {
    (n + 3) & !3
}

impl Fdt {
    /// # Safety
    /// `ptr` must be what the boot protocol put in x0: a device tree blob that
    /// stays mapped and unmodified for the life of the kernel.
    pub unsafe fn from_ptr(ptr: *const u8) -> Option<Fdt> {
        if ptr.is_null() {
            return None;
        }
        // Read the header through a deliberately short slice first: the total
        // size is inside the header, so trusting it before checking the magic
        // would mean mapping an arbitrary length out of a bad pointer.
        let head = core::slice::from_raw_parts(ptr, 40);
        if be32(head, 0) != MAGIC {
            return None;
        }
        let total = be32(head, 4) as usize;
        // A DTB smaller than its own header, or implausibly large, is a
        // pointer that survived the magic check by accident.
        if total < 40 || total > 64 * 1024 * 1024 {
            return None;
        }
        let blob = core::slice::from_raw_parts(ptr, total);
        let struct_off = be32(blob, 8) as usize;
        let strings_off = be32(blob, 12) as usize;
        let struct_size = be32(blob, 36) as usize;
        if struct_off + struct_size > total || strings_off > total {
            return None;
        }
        Some(Fdt { blob, struct_off, struct_size, strings_off })
    }

    pub fn total_size(&self) -> usize {
        self.blob.len()
    }

    pub fn base(&self) -> usize {
        self.blob.as_ptr() as usize
    }

    /// Visit every node, in tree order. `f` returning true stops the walk --
    /// which is how the find helpers below avoid parsing the rest of the tree
    /// once they have their answer.
    pub fn walk<'a>(&'a self, mut f: impl FnMut(usize, Node<'a>) -> bool) {
        let end = self.struct_off + self.struct_size;
        let mut at = self.struct_off;
        let mut depth = 0usize;
        while at + 4 <= end {
            match be32(self.blob, at) {
                FDT_BEGIN_NODE => {
                    let name_off = at + 4;
                    let props_off = align4(name_off + cstr(self.blob, name_off).len() + 1);
                    let node = Node { fdt: self, name_off, props_off };
                    if f(depth, node) {
                        return;
                    }
                    depth += 1;
                    at = props_off;
                }
                FDT_END_NODE => {
                    depth = depth.saturating_sub(1);
                    at += 4;
                }
                FDT_PROP => {
                    let len = be32(self.blob, at + 4) as usize;
                    at += 12 + align4(len);
                }
                FDT_NOP => at += 4,
                FDT_END => return,
                // An unrecognised token means the walk has lost its place, and
                // continuing would read whatever happens to follow as a tree.
                _ => return,
            }
        }
    }

    /// The first node whose `compatible` list contains `compat`.
    ///
    /// By compatible string rather than by path, because a path is a promise
    /// about how one machine's device tree happens to be laid out and a
    /// compatible string is the binding itself. `/pl011@9000000` is true of
    /// QEMU virt and of nothing else.
    pub fn find_compatible(&self, compat: &str) -> Option<Node<'_>> {
        let mut found = None;
        self.walk(|_, n| {
            if n.is_compatible(compat) {
                found = Some(n);
                true
            } else {
                false
            }
        });
        found
    }

    /// Every node with this compatible string. `virtio,mmio` alone matches 32
    /// of them on `virt`.
    pub fn each_compatible<'a>(&'a self, compat: &str, mut f: impl FnMut(Node<'a>)) {
        self.walk(|_, n| {
            if n.is_compatible(compat) {
                f(n);
            }
            false
        });
    }

    /// The first node whose name begins with `prefix`, e.g. "memory@".
    pub fn find_by_prefix(&self, prefix: &str) -> Option<Node<'_>> {
        let mut found = None;
        self.walk(|_, n| {
            if n.name().starts_with(prefix) {
                found = Some(n);
                true
            } else {
                false
            }
        });
        found
    }

    /// #address-cells and #size-cells from the root node.
    ///
    /// Only the root's, which is a real limitation and the right one for now:
    /// on QEMU `virt` every device is a direct child of root, so root's cells
    /// govern every `reg` this kernel reads. A machine that puts devices
    /// behind a `/soc` bus with different cell counts will read nonsense here,
    /// and the fix then is to carry the cells down the walk rather than to
    /// patch a special case in.
    pub fn root_cells(&self) -> (u32, u32) {
        let mut cells = (2, 1); // the specification's defaults
        self.walk(|depth, n| {
            if depth != 0 {
                return true; // past the root; nothing else can answer this
            }
            if let Some(v) = n.prop_u32("#address-cells") {
                cells.0 = v;
            }
            if let Some(v) = n.prop_u32("#size-cells") {
                cells.1 = v;
            }
            false
        });
        cells
    }
}

impl<'a> Node<'a> {
    pub fn name(&self) -> &'a str {
        cstr(self.fdt.blob, self.name_off)
    }

    /// The raw bytes of a property, or None. Scans only this node's own
    /// properties: they are the tokens between the node's name and whatever
    /// comes next, so the scan stops at the first child or at the node's end.
    pub fn prop(&self, want: &str) -> Option<&'a [u8]> {
        let b = self.fdt.blob;
        let mut at = self.props_off;
        loop {
            match be32(b, at) {
                FDT_PROP => {
                    let len = be32(b, at + 4) as usize;
                    let name = cstr(b, self.fdt.strings_off + be32(b, at + 8) as usize);
                    if name == want {
                        return Some(&b[at + 12..at + 12 + len]);
                    }
                    at += 12 + align4(len);
                }
                FDT_NOP => at += 4,
                _ => return None, // BEGIN_NODE, END_NODE or END: out of properties
            }
        }
    }

    pub fn prop_u32(&self, want: &str) -> Option<u32> {
        let v = self.prop(want)?;
        (v.len() >= 4).then(|| be32(v, 0))
    }

    /// `compatible` is a list of NUL-separated strings, most specific first.
    pub fn is_compatible(&self, compat: &str) -> bool {
        let Some(v) = self.prop("compatible") else { return false };
        v.split(|&b| b == 0)
            .any(|s| core::str::from_utf8(s).map(|s| s == compat).unwrap_or(false))
    }

    /// The `index`th (address, size) pair of `reg`, using the root's cell
    /// counts. Returns None when the property is missing or too short, rather
    /// than a zero address that would look like a legitimate answer.
    pub fn reg(&self, index: usize) -> Option<(u64, u64)> {
        let (ac, sc) = self.fdt.root_cells();
        let v = self.prop("reg")?;
        let stride = (ac + sc) as usize * 4;
        let at = index * stride;
        if at + stride > v.len() {
            return None;
        }
        let read = |off: usize, cells: u32| -> u64 {
            let mut n = 0u64;
            for i in 0..cells as usize {
                n = (n << 32) | be32(v, off + i * 4) as u64;
            }
            n
        };
        Some((read(at, ac), read(at + ac as usize * 4, sc)))
    }

    /// One cell of `interrupts`. The arm,gic-v3 binding uses three cells per
    /// interrupt: type (0 = SPI, 1 = PPI), number, flags -- and the number is
    /// relative to the type's base, not an absolute INTID.
    pub fn interrupt(&self, index: usize) -> Option<(u32, u32, u32)> {
        let v = self.prop("interrupts")?;
        let at = index * 12;
        (at + 12 <= v.len()).then(|| (be32(v, at), be32(v, at + 4), be32(v, at + 8)))
    }
}
