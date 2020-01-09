// Copyright 2019 Authors of Red Sift
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

/*!
XDP (eXpress Data Path).

XDP provides high performance network processing capabilities in the kernel.
For an overview of XDP and how it works, see
<https://www.iovisor.org/technology/xdp>.

# Example

Block all traffic directed to port 80:

```
#![no_std]
#![no_main]
use redbpf_probes::bindings::*;
use redbpf_probes::xdp::{XdpAction, XdpContext};
use redbpf_macros::{program, xdp};

program!(0xFFFFFFFE, "GPL");

#[xdp]
pub extern "C" fn block_port_80(ctx: XdpContext) -> XdpAction {
    if let Some(transport) = ctx.transport() {
        if transport.dest() == 80 {
            return XdpAction::Drop;
        }
    }

    XdpAction::Pass
}
```
 */
use core::mem;
use core::slice;
use cty::*;

use crate::bindings::*;
use crate::maps::{PerfMap as PerfMapBase, PerfMapFlags};

/// The return type of XDP probes.
#[repr(u32)]
pub enum XdpAction {
    /// Signals that the program had an unexpected anomaly. Should only be used
    /// for debugging purposes.
    ///
    /// Results in the packet being dropped.
    Aborted = xdp_action_XDP_ABORTED,
    /// The simplest and fastest action. It simply instructs the driver to drop
    /// the packet.
    Drop = xdp_action_XDP_DROP,
    /// Pass the packet to the normal network stack for processing. Note that the
    /// XDP program is allowed to have modified the packet-data.
    Pass = xdp_action_XDP_PASS,
    /// Results in TX bouncing the received packet back to the same NIC it
    /// arrived on. This is usually combined with modifying the packet contents
    /// before returning.
    Tx = xdp_action_XDP_TX,
    /// Similar to `Tx`, but through another NIC.
    Redirect = xdp_action_XDP_REDIRECT,
}

/// The packet transport header.
///
/// Currently only `TCP` and `UDP` transports are supported.
pub enum Transport {
    TCP(*const tcphdr),
    UDP(*const udphdr),
}

impl Transport {
    /// Returns the source port.
    #[inline]
    pub fn source(&self) -> u16 {
        let source = match *self {
            Transport::TCP(hdr) => unsafe { (*hdr).source },
            Transport::UDP(hdr) => unsafe { (*hdr).source },
        };
        u16::from_be(source)
    }

    /// Returns the destination port.
    #[inline]
    pub fn dest(&self) -> u16 {
        let dest = match *self {
            Transport::TCP(hdr) => unsafe { (*hdr).dest },
            Transport::UDP(hdr) => unsafe { (*hdr).dest },
        };
        u16::from_be(dest)
    }
}

/// Context object provided to XDP programs.
///
/// XDP programs are passed a `XdpContext` instance as their argument. Through
/// the context, programs can inspect and modify the packet.
pub struct XdpContext {
    pub ctx: *mut xdp_md,
}

impl XdpContext {
    /// Returns the raw `xdp_md` context.
    #[inline]
    pub fn inner(&self) -> *mut xdp_md {
        self.ctx
    }

    /// Returns the packet length.
    #[inline]
    pub fn len(&self) -> u32 {
        unsafe {
            let ctx = *self.ctx;
            ctx.data_end - ctx.data
        }
    }

    /// Returns the packet's `Ethernet` header if present.
    #[inline]
    pub fn eth(&self) -> Option<*const ethhdr> {
        let ctx = unsafe { *self.ctx };
        let eth = ctx.data as *const ethhdr;
        let end = ctx.data_end as *const c_void;
        unsafe {
            if eth.add(1) as *const c_void > end {
                return None;
            }
        }
        Some(eth)
    }

    /// Returns the packet's `IP` header if present.
    #[inline]
    pub fn ip(&self) -> Option<*const iphdr> {
        let eth = self.eth()?;
        unsafe {
            if (*eth).h_proto != u16::from_be(ETH_P_IP as u16) {
                return None;
            }

            let ip = eth.add(1) as *const iphdr;
            if ip.add(1) as *const c_void > (*self.ctx).data_end as *const c_void {
                return None;
            }
            Some(ip)
        }
    }

    /// Returns the packet's transport header if present.
    #[inline]
    pub fn transport(&self) -> Option<Transport> {
        unsafe {
            let ip = self.ip()?;
            let base = (ip as *const u8).add(((*ip).ihl() * 4) as usize);
            let (transport, size) = match (*ip).protocol as u32 {
                IPPROTO_TCP => (Transport::TCP(base.cast()), mem::size_of::<tcphdr>()),
                IPPROTO_UDP => (Transport::UDP(base.cast()), mem::size_of::<udphdr>()),
                _ => return None,
            };
            if base.add(size) > (*self.ctx).data_end as *const u8 {
                return None;
            }
            Some(transport)
        }
    }

    /// Returns the packet's data starting after the transport headers.
    #[inline]
    pub fn data(&self) -> Option<Data> {
        use Transport::*;
        unsafe {
            let base = match self.transport()? {
                TCP(hdr) => {
                    if hdr.add(1) as *const u8 > (*self.ctx).data_end as *const u8 {
                        return None;
                    }
                    let mut base = hdr.add(1) as *const u8;
                    let data_offset = (*hdr).doff();
                    if data_offset > 5 {
                        base = base.add(((data_offset - 5) * 4) as usize);
                    }
                    base
                }
                UDP(hdr) => hdr.add(1) as *const u8,
            };
            if base > (*self.ctx).data_end as *const u8 {
                return None;
            }
            Some(Data {
                ctx: self.ctx,
                base,
            })
        }
    }
}

/// Data type returned by calling `XdpContext::data()`
pub struct Data {
    ctx: *const xdp_md,
    base: *const u8,
}

impl Data {
    /// Returns the offset from the first byte of the packet.
    #[inline]
    pub fn offset(&self) -> usize {
        unsafe { (self.base as u32 - (*self.ctx).data) as usize }
    }

    /// Returns the length of the data.
    ///
    /// This is equivalent to the length of the packet minus the length of the headers.
    #[inline]
    pub fn len(&self) -> usize {
        unsafe { ((*self.ctx).data_end - self.base as u32) as usize }
    }

    /// Returns a `slice` of `len` bytes from the data.
    #[inline]
    pub fn slice(&self, len: usize) -> Option<&[u8]> {
        let full_slice = unsafe {
            // SAFETY: base pointer should be valid for our own length.
            slice::from_raw_parts(self.base, self.len())
        };

        full_slice.get(..len)
    }

    #[inline]
    pub fn read<T>(&self) -> Option<T> {
        let bytes = self.slice(mem::size_of::<T>())?;

        unsafe {
            Some((bytes.as_ptr() as *const T).read_unaligned())
        }
    }
}

/// Convenience data type to exchange payload data.
#[repr(C)]
pub struct MapData<T> {
    /// The custom data type to be exchanged with user space.
    pub data: T,
    offset: u32,
    size: u32,
    payload: [u8; 0],
}

impl<T> MapData<T> {
    /// Create a new `MapData` value that includes only `data` and no packet
    /// payload.
    pub fn new(&self, data: T) -> Self {
        MapData::<T>::with_payload(data, 0, 0)
    }

    /// Create a new `MapData` value that includes `data` and `size` payload
    /// bytes, where the interesting part of the payload starts at `offset`.
    ///
    /// The payload can then be retrieved calling `MapData::payload()`.
    pub fn with_payload(data: T, offset: u32, size: u32) -> Self {
        Self {
            data,
            payload: [],
            offset,
            size
        }
    }

    /// Return the payload if any, skipping the initial `offset` bytes.
    pub fn payload(&self) -> &[u8] {
        unsafe {
            let base = self.payload.as_ptr().add(self.offset as usize);
            slice::from_raw_parts(base, (self.size - self.offset) as usize)
        }
    }
}

/// Perf events map.
///
/// Similar to `PerfMap`, with additional XDP-only API.
#[repr(transparent)]
pub struct PerfMap<T>(PerfMapBase<MapData<T>>);

impl<T> PerfMap<T> {
    /// Creates a perf map with the specified maximum number of elements.
    pub const fn with_max_entries(max_entries: u32) -> Self {
        Self(PerfMapBase::with_max_entries(max_entries))
    }

    /// Insert a new event in the perf events array keyed by the current CPU number.
    ///
    /// Each array can hold up to `max_entries` events, see `with_max_entries`.
    /// If you want to use a key other than the current CPU, see
    /// `insert_with_flags`.
    ///
    /// `packet_size` specifies the number of bytes from the current packet that
    /// the kernel should append to the event data.
    #[inline]
    pub fn insert(&mut self, ctx: &XdpContext, data: MapData<T>) {
        let size = data.size;
        self.0
            .insert_with_flags(ctx.inner(), data, PerfMapFlags::with_xdp_size(size))
    }

    /// Insert a new event in the perf events array keyed by the index and with
    /// the additional xdp payload data specified in the given `PerfMapFlags`.
    #[inline]
    pub fn insert_with_flags(&mut self, ctx: &XdpContext, data: MapData<T>, mut flags: PerfMapFlags) {
        flags.xdp_size = data.size;
        self.0.insert_with_flags(ctx.inner(), data, flags)
    }
}
