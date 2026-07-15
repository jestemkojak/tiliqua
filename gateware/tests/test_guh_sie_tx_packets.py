# Copyright (c) 2024 S. Holzapfel <me@sebholzapfel.com>
#
# SPDX-License-Identifier: BSD-3-Clause
"""
Wire-level check of the guh SIE's bulk-OUT transmit path: drive the REAL
USBSIE (real token generator, packet generator, CRC engine, SOF timing) and
capture the byte stream it presents to the UTMI TX interface, then verify
packet structure and CRC16 in Python.

Motivated by the 2026-07-15 M6b hardware evidence: 31-byte CBW OUT packets
are ACKed by a real drive while the write's 64-byte data OUT packets get NO
handshake at all (SIE response = TIMEOUT). Per USB 2.0, a device stays silent
exactly when the packet it received was corrupt — so the question is whether
the SIE emits a well-formed max-size (64-byte) packet. All previous sims
stubbed the SIE out; this test exercises it for real, minus only the ULPI
translator and PHY.
"""

import unittest

from amaranth import *
from amaranth.sim import Simulator

from luna.gateware.interface.utmi import UTMIInterface

# The vendored SIE (the one production actually elaborates — SCSIBulkHost
# swaps it into the enumerator) — stock-guh-equivalent except for NYET decode.
from vendor.guh_msc.sie import USBSIE, TransferType, TransferResponse, DataPID
from guh.util.test_util import connect_utmi, patch_usb_timing_for_simulation


def usb_crc16(data):
    """USB CRC16 (poly 0x8005 reflected: 0xA001), init 0xFFFF, complemented,
    transmitted LSB-first => little-endian byte order on the wire."""
    crc = 0xFFFF
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return (~crc) & 0xFFFF


def usb_crc5_token(data11):
    """USB CRC5 over the 11-bit token payload (addr|endp), complemented."""
    crc = 0x1F
    for i in range(11):
        bit = (data11 >> i) & 1
        if bit ^ (crc & 1):
            crc = (crc >> 1) ^ 0x14
        else:
            crc >>= 1
    return (~crc) & 0x1F


PID_OUT = 0xE1
PID_SOF = 0xA5
PID_DATA0 = 0xC3
PID_DATA1 = 0x4B
PID_ACK = 0xD2
PID_NYET = 0x96


class GuhSieTxPacketTests(unittest.TestCase):

    def _run_out_transfer(self, payload_len, data_pid=DataPID.DATA0,
                          handshake_byte=None):
        """Issue one bulk-OUT transfer of `payload_len` pattern bytes through
        the real SIE; return the list of captured TX packets (SOFs filtered),
        plus the payload that was loaded. If `handshake_byte` is given, the
        device side transmits that single-byte handshake packet right after
        the host's data packet ends (instead of staying silent)."""
        patch_usb_timing_for_simulation()

        sie = USBSIE(bus=None)
        dev_utmi = UTMIInterface()   # silent dummy device side

        m = Module()
        m.submodules.sie = sie
        connect_utmi(m, sie.utmi, dev_utmi)

        payload = [(i * 5 + 7) & 0xFF for i in range(payload_len)]
        packets = []   # list of byte lists
        result = {}

        async def tb(ctx):
            c = sie.ctrl
            # Wait for bus reset to finish (patched timings).
            for _ in range(2_000_000):
                if ctx.get(c.status.idle):
                    break
                await ctx.tick("usb")
            else:
                self.fail("SIE never became idle after reset")

            # Load payload bytes into the SIE TX FIFO.
            for b in payload:
                ctx.set(c.txs.valid, 1)
                ctx.set(c.txs.payload, b)
                while not ctx.get(c.txs.ready):
                    await ctx.tick("usb")
                await ctx.tick("usb")
            ctx.set(c.txs.valid, 0)

            # Issue the OUT transfer.
            ctx.set(c.xfer.type, TransferType.OUT)
            ctx.set(c.xfer.dev_addr, 0x12)
            ctx.set(c.xfer.ep_addr, 2)
            ctx.set(c.xfer.data_pid, data_pid)
            ctx.set(c.xfer.start, 1)
            await ctx.tick("usb")
            ctx.set(c.xfer.start, 0)

            # Capture TX bytes until the SIE returns to idle (the missing
            # device means the handshake wait times out on its own).
            cur = []
            gap = 0
            injected = False
            for _ in range(2_000_000):
                if ctx.get(sie.utmi.tx_valid) and ctx.get(sie.utmi.tx_ready):
                    cur.append(ctx.get(sie.utmi.tx_data))
                    gap = 0
                else:
                    gap += 1
                    if cur and gap > 8:      # packet boundary
                        packets.append(cur)
                        cur = []
                        # After token+data have gone out, answer with the
                        # requested device handshake (single-PID packet).
                        nonsof = [p for p in packets if p[0] != PID_SOF]
                        if (handshake_byte is not None and not injected
                                and len(nonsof) >= 2):
                            ctx.set(dev_utmi.tx_valid, 1)
                            ctx.set(dev_utmi.tx_data, handshake_byte)
                            while not ctx.get(dev_utmi.tx_ready):
                                await ctx.tick("usb")
                            await ctx.tick("usb")
                            ctx.set(dev_utmi.tx_valid, 0)
                            injected = True
                if ctx.get(c.status.idle) and not cur and packets:
                    break
                await ctx.tick("usb")
            result["response"] = ctx.get(c.status.response)

        sim = Simulator(m)
        sim.add_clock(1 / 60e6, domain="usb")
        # connect_utmi's preamble counter runs in the sync domain — without
        # this clock, tx_ready never asserts and nothing transmits.
        sim.add_clock(1 / 60e6, domain="sync")
        sim.add_testbench(tb)
        sim.run()

        # Filter out SOF keepalive packets.
        packets = [p for p in packets if p[0] != PID_SOF]
        return packets, payload, result

    def _check_out_exchange(self, payload_len, data_pid=DataPID.DATA0):
        packets, payload, result = self._run_out_transfer(payload_len, data_pid)
        self.assertGreaterEqual(
            len(packets), 2,
            f"expected token+data packets, captured: {packets}")
        token, data = packets[0], packets[1]

        # Token: PID_OUT + 2 bytes (addr[6:0] | endp[3:0] | crc5).
        self.assertEqual(token[0], PID_OUT)
        self.assertEqual(len(token), 3, f"malformed token: {token}")
        tok11 = (token[1] | (token[2] << 8)) & 0x7FF
        self.assertEqual(tok11 & 0x7F, 0x12)             # address
        self.assertEqual((tok11 >> 7) & 0xF, 2)          # endpoint
        self.assertEqual((token[2] >> 3) & 0x1F,
                         usb_crc5_token(tok11), "token CRC5 wrong")

        # Data: DATA0/DATA1 PID + payload + CRC16 (little-endian).
        want_pid = PID_DATA1 if data_pid == DataPID.DATA1 else PID_DATA0
        self.assertEqual(
            data[0], want_pid,
            f"data packet PID wrong for {payload_len}B payload with "
            f"{data_pid!r}: {data[0]:#x} (want {want_pid:#x})")
        self.assertEqual(
            len(data), 1 + payload_len + 2,
            f"data packet length wrong for {payload_len}B payload: "
            f"{len(data)} bytes")
        self.assertEqual(
            data[1:1 + payload_len], payload,
            f"payload corrupted for {payload_len}B packet")
        crc = usb_crc16(payload)
        self.assertEqual(
            data[1 + payload_len] | (data[2 + payload_len] << 8), crc,
            f"CRC16 wrong for {payload_len}B packet")

    def test_out_packet_31_bytes_cbw_sized(self):
        """Baseline: CBW-sized OUT packets are known-good on hardware."""
        self._check_out_exchange(31)

    def test_out_packet_64_bytes_max_size(self):
        """The suspect: max-packet-size OUT data (SIE TX FIFO exactly full) —
        the first 64-byte OUT the SIE ever transmits on this design."""
        self._check_out_exchange(64)

    def test_out_packet_32_bytes_data1_pid(self):
        """The hardware failure case exactly: the write's data packet goes out
        right after the ACKed CBW toggles pid_out, i.e. as DATA1 — a PID this
        sim previously never exercised (2026-07-15 rej=4/2/0 at 32B chunks)."""
        self._check_out_exchange(32, DataPID.DATA1)

    def test_out_packet_64_bytes_data1_pid(self):
        """Max-size packet with the toggled PID, for completeness."""
        self._check_out_exchange(64, DataPID.DATA1)

    def test_out_packet_31_bytes_data1_pid(self):
        """CBW-sized with DATA1: hardware ACKs these (alternate READ(10)
        keepalive CBWs), so this must pass — a failure here would mean the
        sim harness, not the SIE, is wrong."""
        self._check_out_exchange(31, DataPID.DATA1)

    def test_out_data_ack_handshake_reported(self):
        """Harness control: a device ACK after the data packet must be
        decoded as ACK — proves the handshake-injection path itself works
        before the NYET test below can mean anything."""
        _, _, result = self._run_out_transfer(
            31, DataPID.DATA1, handshake_byte=PID_ACK)
        self.assertEqual(result["response"], TransferResponse.ACK)

    def test_out_data_nyet_handshake_reported(self):
        """The 2026-07-15 hardware failure: a High-Speed device answering a
        bulk-OUT data packet with NYET (data ACCEPTED, endpoint busy — USB
        2.0 §8.5.1, flash drives do this routinely on writes). Stock guh
        dropped it into the TIMEOUT arm (rej=4/2/0 on hardware); the vendored
        SIE must report it distinctly so the MSC engine can advance."""
        _, _, result = self._run_out_transfer(
            31, DataPID.DATA1, handshake_byte=PID_NYET)
        self.assertEqual(result["response"], TransferResponse.NYET)


if __name__ == "__main__":
    unittest.main()
