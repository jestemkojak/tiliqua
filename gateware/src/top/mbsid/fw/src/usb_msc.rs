//! USBMSCPeripheral CSR driver: block reads from a USB mass-storage device.
use tiliqua_pac as pac;

/// TEMPORARY (M6b §7b bring-up): per-call outcome counters, read by main.rs's
/// UART0 diagnostic prints to localize where an export dies without adding
/// any USB traffic. Cell is fine: only the main loop touches UsbMsc.
/// Remove together with the main.rs diag prints once §7b passes.
#[derive(Default)]
pub struct MscDiag {
    pub rd: core::cell::Cell<u32>,
    pub rd_err: core::cell::Cell<u32>,
    pub wr: core::cell::Cell<u32>,
    pub wr_ok: core::cell::Cell<u32>,
    pub wr_notready: core::cell::Cell<u32>,
    pub wr_resp_err: core::cell::Cell<u32>,
    pub wr_timeout: core::cell::Cell<u32>,
    pub wr_spins_last: core::cell::Cell<u32>,
    /// Last reject info after a failed write:
    /// (SIE response, phase, txdone, nyets, last_phase).
    /// response: 3=STALL, 4=TIMEOUT, 5=CRC_ERROR; phase: 1=CBW, 2=DATA-TX,
    /// 3=CSW, 4=DATA-RX, 5=CTRL (clear-halt recovery); txdone: 32-byte units
    /// ACKed before a DATA-TX reject; nyets: NYET handshakes during the
    /// command (HS flow control seen); last_phase: last live engine phase —
    /// on a watchdogged wedge (rej response/phase = 0) this is the phase the
    /// engine was STUCK in. All five survive the engine's watchdog reset
    /// (latched CSR-side, see usb_msc_csr.py RejectInfo).
    pub wr_reject: core::cell::Cell<(u8, u8, u8, u8, u8)>,
    /// Last CSW seen after a failed/timed-out write: (status, residue).
    /// residue > 0 => the device did not accept the whole 512-byte data
    /// phase (host-side data-phase bug); residue == 0 with status=1 => the
    /// device took the data and refused the write (e.g. write-protect).
    pub wr_csw: core::cell::Cell<(u8, u32)>,
    /// FIRST failure since begin() — the last-failure cells above get
    /// overwritten by retries (which is how the 2026-07-15 all-zeros log
    /// destroyed its own evidence: the final attempt's post-watchdog-reset
    /// snapshot masked the earlier real CSW failures).
    pub wr_csw_first: core::cell::Cell<(u8, u32)>,
    pub wr_reject_first: core::cell::Cell<(u8, u8, u8, u8, u8)>,
    pub wr_first_set: core::cell::Cell<bool>,
    /// TEMPORARY round-six: first/last failed READ since begin():
    /// (reason: 1=not-ready at entry, 2=resp.error, 3=spin timeout;
    ///  word index 0..127 the failure hit at; lba; spins in the failing
    ///  word's poll loop; then a reject_info CSR snapshot at the moment of
    ///  failure: response, phase, nyets, last_phase). Round six evidence:
    ///  the first read after the first successful write fails
    ///  deterministically on hardware while the same sequence passes in
    ///  sim — these cells discriminate drive-busy (3 with high spins) from
    ///  engine-level rejection (2, with the reject snapshot saying where).
    pub rd_fail_first: core::cell::Cell<(u8, u8, u32, u32, u8, u8, u8, u8)>,
    pub rd_fail: core::cell::Cell<(u8, u8, u32, u32, u8, u8, u8, u8)>,
    /// Packed read_path_info CSR captured with rd_fail_first/rd_fail.
    /// [9:0]=engine bytes, [19:10]=peripheral bytes,
    /// [27:20]=peripheral words, [28]=stream mode,
    /// [29]=sampled data length was 512.
    pub rd_path_first: core::cell::Cell<u32>,
    pub rd_path: core::cell::Cell<u32>,
    pub rd_first_set: core::cell::Cell<bool>,
}

impl MscDiag {
    /// Reset the first-failure capture (call at the start of an operation
    /// whose failures you want to attribute, e.g. one export attempt).
    pub fn begin(&self) {
        self.wr_first_set.set(false);
        self.wr_csw_first.set((0, 0));
        self.wr_reject_first.set((0, 0, 0, 0, 0));
        self.rd_first_set.set(false);
        self.rd_fail_first.set((0, 0, 0, 0, 0, 0, 0, 0));
        self.rd_fail.set((0, 0, 0, 0, 0, 0, 0, 0));
        self.rd_path_first.set(0);
        self.rd_path.set(0);
    }

    fn record_rd_failure(
        &self,
        v: (u8, u8, u32, u32, u8, u8, u8, u8),
        path: u32,
    ) {
        self.rd_fail.set(v);
        self.rd_path.set(path);
        if !self.rd_first_set.get() {
            self.rd_first_set.set(true);
            self.rd_fail_first.set(v);
            self.rd_path_first.set(path);
        }
    }

    fn record_failure(&self, csw: (u8, u32), reject: (u8, u8, u8, u8, u8)) {
        self.wr_csw.set(csw);
        self.wr_reject.set(reject);
        if !self.wr_first_set.get() {
            self.wr_first_set.set(true);
            self.wr_csw_first.set(csw);
            self.wr_reject_first.set(reject);
        }
    }
}

pub struct UsbMsc { regs: pac::USB_MSC, pub diag: MscDiag }

#[derive(Debug)]
pub enum MscError { NotReady, ReadError, WriteError }

impl UsbMsc {
    pub fn new(regs: pac::USB_MSC) -> Self {
        Self { regs, diag: MscDiag::default() }
    }

    pub fn ready(&self) -> bool {
        self.regs.status().read().ready().bit_is_set()
    }

    pub fn connected(&self) -> bool {
        self.regs.status().read().connected().bit_is_set()
    }

    /// Mirror of the menu's USB Mode row: 1 = MSC owns the PHY (Storage).
    pub fn set_mode(&self, storage: bool) {
        self.regs.mode().write(|w| w.storage().bit(storage));
    }

    /// Negotiated link speed (guh USBHostSpeed = LUNA xcvr_select encoding:
    /// 0=HIGH, 1=FULL, 2=LOW, 3=UNKNOWN/no device). A USB 3 stick on this
    /// port reads 0 (High Speed) — NYET territory.
    pub fn speed(&self) -> u8 {
        self.regs.status().read().speed().bits()
    }

    pub fn block_size(&self) -> u16 {
        self.regs.block_size().read().value().bits()
    }

    /// Read one 512-byte block at `lba` into `buf`. Callers must have checked
    /// `block_size() == 512`: the fixed 128-word drain (and the gateware's
    /// non-backpressuring byte packer) silently corrupts any other sector size.
    /// TEMPORARY round-six diag: reject_info CSR snapshot (response, phase,
    /// nyets, last_phase) captured at a read-failure site.
    fn reject_snapshot(&self) -> (u8, u8, u8, u8) {
        let ri = self.regs.reject_info().read();
        (ri.response().bits(), ri.phase().bits(),
         ri.nyets().bits(), ri.last_phase().bits())
    }

    fn read_path_snapshot(&self) -> u32 {
        let p = self.regs.read_path_info().read();
        u32::from(p.engine_bytes().bits())
            | (u32::from(p.periph_bytes().bits()) << 10)
            | (u32::from(p.periph_words().bits()) << 20)
            | (u32::from(p.stream_mode().bit_is_set()) << 28)
            | (u32::from(p.data_len_512().bit_is_set()) << 29)
    }

    pub fn read_block(&self, lba: u32, buf: &mut [u8; 512]) -> Result<(), MscError> {
        self.diag.rd.set(self.diag.rd.get().wrapping_add(1)); // TEMPORARY diag
        if !self.ready() {
            self.diag.rd_err.set(self.diag.rd_err.get().wrapping_add(1));
            let (rr, rp, ny, lp) = self.reject_snapshot();
            let path = self.read_path_snapshot();
            self.diag.record_rd_failure(
                (1, 0, lba, 0, rr, rp, ny, lp), path);
            return Err(MscError::NotReady);
        }
        self.regs.lba().write(|w| unsafe { w.value().bits(lba) });
        self.regs.start().write(|w| w.strobe().set_bit());
        // Drain 128 words (512 bytes). Spin on rx_avail per word.
        for i in 0..128usize {
            // Cap spin iterations to prevent an infinite hang if the device
            // stalls (no word, no error). Raised 1M -> 10M (~0.3 s -> ~3 s)
            // after round six (2026-07-15): a drive that just accepted a
            // write busy-NAKs subsequent reads for seconds while its FTL
            // commits — the 8GB stick's export failed on exactly this
            // (d_wrok=1 then d_rderr=2 on the verify readback).
            #[cfg(not(test))]
            const MAX_SPIN: u32 = 10_000_000;
            #[cfg(not(test))]
            let mut spins: u32 = 0;
            loop {
                let st = self.regs.status().read();
                if st.rx_avail().bit_is_set() { break; }
                if self.regs.resp().read().error().bit_is_set() {
                    self.diag.rd_err.set(self.diag.rd_err.get().wrapping_add(1));
                    #[cfg(not(test))]
                    let sp = spins;
                    #[cfg(test)]
                    let sp = 0u32;
                    let (rr, rp, ny, lp) = self.reject_snapshot();
                    let path = self.read_path_snapshot();
                    self.diag.record_rd_failure(
                        (2, i as u8, lba, sp, rr, rp, ny, lp), path);
                    return Err(MscError::ReadError);
                }
                #[cfg(not(test))]
                {
                    spins += 1;
                    if spins >= MAX_SPIN {
                        self.diag.rd_err.set(self.diag.rd_err.get().wrapping_add(1));
                        let (rr, rp, ny, lp) = self.reject_snapshot();
                        let path = self.read_path_snapshot();
                        self.diag.record_rd_failure(
                            (3, i as u8, lba, spins, rr, rp, ny, lp), path);
                        return Err(MscError::ReadError);
                    }
                }
            }
            let word = self.regs.rx_data().read().word().bits();
            buf[i*4..i*4+4].copy_from_slice(&word.to_le_bytes());
        }
        Ok(())
    }

    /// Auto-REQUEST-SENSE result after a failed write: (valid, key, asc,
    /// ascq). key=0x7/asc=0x27 = WRITE PROTECTED; key=0x2 = NOT READY.
    pub fn sense_info(&self) -> (bool, u8, u8, u8) {
        let r = self.regs.sense_info().read();
        let code = r.code().bits();
        (r.valid().bit_is_set(),
         ((code >> 16) & 0xF) as u8,
         ((code >> 8) & 0xFF) as u8,
         (code & 0xFF) as u8)
    }

    /// Write one 512-byte block at `lba`. Same block_size()==512 precondition
    /// as read_block. Contract (revised after the 2026-07-14 incident):
    /// lba -> start_write strobe (arms: flushes leftover TX words, clears
    /// sticky resp) -> 128 tx words -> the gateware starts the engine by
    /// itself once the 128th word is banked -> poll sticky resp.done, then
    /// resp.error.
    ///
    /// **History (2026-07-14): the original fill-then-strobe contract
    /// corrupted a real drive.** The gateware flushed the TX FIFO on the
    /// same strobe that started the write, so every WRITE(10) went out with
    /// an empty payload and left the device hanging mid-command (bulk-only
    /// transport desync -> mostly-zero sectors written at arbitrary LBAs).
    /// Root-caused and fixed in gateware simulation
    /// (`tests/test_usb_msc_csr.py::test_write_contract_strobe_then_fill_defers_start`);
    /// the export path is re-enabled for hardware validation, which must run
    /// on **disposable media only** until the `M6_USB_STORAGE.md` §7b
    /// checklist passes (§8 risk table).
    pub fn write_block(&self, lba: u32, buf: &[u8; 512]) -> Result<(), MscError> {
        self.diag.wr.set(self.diag.wr.get().wrapping_add(1)); // TEMPORARY diag
        // Bounded ready-wait instead of an instant NotReady: after a failed
        // write the engine stays busy for a few ms running its automatic
        // REQUEST SENSE; an immediate retry must wait that out, not fail.
        const READY_SPIN: u32 = 2_000_000; // ~1 s worst case
        let mut waited: u32 = 0;
        while !self.ready() {
            waited += 1;
            if waited >= READY_SPIN {
                self.diag.wr_notready.set(
                    self.diag.wr_notready.get().wrapping_add(1));
                return Err(MscError::NotReady);
            }
        }
        self.regs.lba().write(|w| unsafe { w.value().bits(lba) });
        // Arm FIRST: the strobe flushes stale TX words, so the payload must
        // be pushed after it. The engine start is deferred by gateware until
        // all 128 words are banked.
        self.regs.start_write().write(|w| w.strobe().set_bit());
        for i in 0..128usize {
            let w32 = u32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap());
            self.regs.tx_data().write(|w| unsafe { w.word().bits(w32) });
        }
        // Raised 10M -> 40M (~3 s -> ~12 s) after round six: the 64GB stick
        // accepts the whole data phase (ny=3 = routine HS flow control) then
        // NAKs the CSW for longer than 3 s while committing (lph=3 = engine
        // parked in the CSW phase, no reject). The engine's own 10 s
        // watchdog still bounds a truly-dead exchange; this cap just needs
        // to outlast it so the watchdog (not the poll) decides.
        const MAX_SPIN: u32 = 40_000_000;
        let mut spins: u32 = 0;
        loop {
            let r = self.regs.resp().read();
            if r.done().bit_is_set() {
                self.diag.wr_spins_last.set(spins); // TEMPORARY diag
                return if r.error().bit_is_set() {
                    self.diag.wr_resp_err.set(
                        self.diag.wr_resp_err.get().wrapping_add(1));
                    let ri = self.regs.reject_info().read();
                    self.diag.record_failure(
                        (self.regs.csw_status().read().value().bits(),
                         self.regs.csw_residue().read().value().bits()),
                        (ri.response().bits(), ri.phase().bits(),
                         ri.txdone().bits(), ri.nyets().bits(),
                         ri.last_phase().bits()));
                    Err(MscError::WriteError)
                } else {
                    self.diag.wr_ok.set(self.diag.wr_ok.get().wrapping_add(1));
                    Ok(())
                };
            }
            spins += 1;
            if spins >= MAX_SPIN {
                self.diag.wr_spins_last.set(spins); // TEMPORARY diag
                self.diag.wr_timeout.set(
                    self.diag.wr_timeout.get().wrapping_add(1));
                let ri = self.regs.reject_info().read();
                self.diag.record_failure(
                    (self.regs.csw_status().read().value().bits(),
                     self.regs.csw_residue().read().value().bits()),
                    (ri.response().bits(), ri.phase().bits(),
                     ri.txdone().bits(), ri.nyets().bits(),
                     ri.last_phase().bits()));
                return Err(MscError::WriteError);
            }
        }
    }

}

impl crate::fat::BlockIo for &UsbMsc {
    fn read_block(&mut self, lba: u32, buf: &mut [u8; 512]) -> Result<(), ()> {
        UsbMsc::read_block(self, lba, buf).map_err(|_| ())
    }
    fn write_block(&mut self, lba: u32, buf: &[u8; 512]) -> Result<(), ()> {
        UsbMsc::write_block(self, lba, buf).map_err(|_| ())
    }
}
