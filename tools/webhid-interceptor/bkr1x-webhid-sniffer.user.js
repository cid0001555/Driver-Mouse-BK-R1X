// ==UserScript==
// @name         Fire Phoenix BK-R1X WebHID Protocol Interceptor & Dumper
// @namespace    https://github.com/cid0001555/Driver-Mouse-BK-R1X
// @version      1.1.0
// @description  Hooks navigator.hid and HIDDevice at document-start to capture VID/PID filters, OUT/FEATURE reports, IN telemetry, and UI call stacks on bk-r1x.yjx2012.com.
// @match        https://bk-r1x.yjx2012.com/*
// @match        https://*.yjx2012.com/*
// @run-at       document-start
// @grant        none
// ==/UserScript==

(function () {
  'use strict';

  if (window.__BKR1X_SNIFFER_INSTALLED__) {
    return;
  }
  window.__BKR1X_SNIFFER_INSTALLED__ = true;

  if (!('hid' in navigator) || !navigator.hid) {
    console.warn('[BKR1X-SNIFFER] navigator.hid unavailable (requires Chromium over HTTPS).');
    return;
  }

  const MAX_CAPTURED_PACKETS = 4096;
  const MAX_CAPTURED_FILTERS = 256;
  const HEX_LUT = Array.from({ length: 256 }, (_, i) => i.toString(16).padStart(2, '0'));

  const packetLog = [];
  const filterLog = [];
  let totalPacketSeq = 0;

  const inspectedDevices = new WeakMap();
  const hookedInputDevices = new WeakSet();

  const TX_OPCODES = Object.freeze({
    0x03: 'GET_FIRMWARE_VERSION',
    0x08: 'GET_MOUSE_KEYS',
    0x09: 'SET_MOUSE_KEYS',
    0x0c: 'READ_MACRO_CHUNK',
    0x0d: 'WRITE_MACRO_CHUNK',
    0x0e: 'GET_MOUSE_CONFIG',
    0x0f: 'SET_MOUSE_CONFIG',
    0x10: 'COMMIT_MACRO_FLASH',
    0x21: 'SET_LIGHT_MODE',
    0x30: 'GET_BATTERY_INFO',
    0xed: 'GET_ONLINE_STATUS',
  });

  const RX_OPCODES = Object.freeze({
    0x03: 'ACK_FIRMWARE_VERSION',
    0x08: 'ACK_MOUSE_KEYS',
    0x09: 'ACK_SET_KEYS',
    0x0c: 'ACK_MACRO_CHUNK',
    0x0d: 'ACK_WRITE_MACRO',
    0x0e: 'ACK_MOUSE_CONFIG',
    0x0f: 'ACK_SET_CONFIG',
    0x10: 'ACK_COMMIT_MACRO',
    0x21: 'ACK_LIGHT_MODE',
    0x30: 'ACK_BATTERY_INFO',
    0xe2: 'ASYNC_DONGLE_MATCH',
    0xed: 'ACK_OR_ASYNC_ONLINE_STATUS',
    0xfa: 'ASYNC_TELEMETRY_PUSH',
  });

  function hex8(value) {
    return '0x' + HEX_LUT[value & 0xff].toUpperCase();
  }

  function hex16(value) {
    const v = value & 0xffff;
    return '0x' + (HEX_LUT[(v >>> 8) & 0xff] + HEX_LUT[v & 0xff]).toUpperCase();
  }

  function toUint8Array(source) {
    if (source instanceof Uint8Array) {
      return new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
    }
    if (ArrayBuffer.isView(source)) {
      return new Uint8Array(source.buffer, source.byteOffset, source.byteLength);
    }
    if (source instanceof ArrayBuffer) {
      return new Uint8Array(source);
    }
    return new Uint8Array(0);
  }

  function formatHexBytes(bytes) {
    if (bytes.length === 0) {
      return '';
    }
    const parts = new Array(bytes.length);
    for (let i = 0; i < bytes.length; i++) {
      parts[i] = HEX_LUT[bytes[i]];
    }
    return parts.join(' ');
  }

  function captureCallStack(skipFrames = 3) {
    const raw = new Error().stack || '';
    return raw
      .split('\n')
      .slice(skipFrames)
      .map((line) => line.trim())
      .filter((line) => line.length > 0 && !line.includes('bkr1x-webhid-sniffer'))
      .slice(0, 6);
  }

  function pushBounded(buffer, item, maxLen) {
    if (buffer.length >= maxLen) {
      buffer.shift();
    }
    buffer.push(item);
  }

  function decodeAnnotation(direction, reportId, bytes) {
    if (bytes.length === 0) {
      return 'EMPTY';
    }
    if (direction === 'OUT' || direction === 'FEATURE_OUT') {
      if (bytes[0] === 0x55 && bytes.length >= 2) {
        const op = bytes[1];
        return `TX_MAGIC(0x55) cmd=${hex8(op)} [${TX_OPCODES[op] || 'UNKNOWN_CMD'}]`;
      }
      if (reportId === 0x06) {
        const op = bytes[0];
        return `REPORT_ID(0x06) cmd=${hex8(op)} [${TX_OPCODES[op] || 'MACRO_OP'}]`;
      }
      return 'RAW_FRAME';
    }

    if ((direction === 'IN' || direction === 'FEATURE_IN') && bytes[0] === 0xaa && bytes.length >= 2) {
      const op = bytes[1];
      let suffix = '';
      if (op === 0xfa && bytes.length >= 11) {
        suffix =
          bytes[8] === 0xd0
            ? ` battery=${bytes[9]}% charging=${bytes[10]}`
            : ` dpiStage=${bytes[9]} rateIdx=${bytes[10]}`;
      } else if (op === 0x30 && bytes.length >= 10) {
        suffix = ` battery=${bytes[8]}% charging=${bytes[9]}`;
      } else if (op === 0xed && bytes.length >= 9) {
        suffix = ` online=${bytes[8] === 2}`;
      }
      return `RX_MAGIC(0xAA) op=${hex8(op)} [${RX_OPCODES[op] || 'UNKNOWN_ACK'}]${suffix}`;
    }

    return 'RAW_FRAME';
  }

  function badgeStyleFor(direction) {
    if (direction === 'OUT') {
      return 'color:#ff9e64;font-weight:bold';
    }
    if (direction.startsWith('FEATURE')) {
      return 'color:#bb9af7;font-weight:bold';
    }
    return 'color:#7dcfff;font-weight:bold';
  }

  function recordPacket(device, direction, reportId, bytes, stack) {
    totalPacketSeq += 1;
    const timestamp = new Date().toISOString();
    const perfMs = Number(performance.now().toFixed(3));
    const annotation = decodeAnnotation(direction, reportId, bytes);
    const hex = formatHexBytes(bytes);

    const entry = {
      seq: totalPacketSeq,
      timestamp,
      perfMs,
      direction,
      vendorId: device ? hex16(device.vendorId) : 'N/A',
      productId: device ? hex16(device.productId) : 'N/A',
      productName: device ? device.productName : 'N/A',
      reportId: hex8(reportId),
      length: bytes.length,
      hex,
      annotation,
      stack,
    };
    pushBounded(packetLog, entry, MAX_CAPTURED_PACKETS);

    console.groupCollapsed(
      `%c[BKR1X ${direction}]%c #${entry.seq} @ ${timestamp} | rpt=${entry.reportId} (${bytes.length}B) | ${annotation}`,
      badgeStyleFor(direction),
      'color:#c0caf5;font-weight:normal'
    );
    console.log('Device   :', `${entry.productName} (${entry.vendorId}:${entry.productId})`);
    console.log('Hex Dump :', hex);
    if (stack && stack.length > 0) {
      console.log('CallStack:\n  ' + stack.join('\n  '));
    }
    console.groupEnd();
  }

  const hidProto = window.HIDDevice && window.HIDDevice.prototype;
  const origAddEventListener = hidProto ? hidProto.addEventListener : null;

  // Attaches a single passive listener per HIDDevice so frames are captured whether the page uses
  // `addEventListener('inputreport', ...)` or assigns `device.oninputreport = fn`.
  function attachPassiveInputHook(device) {
    if (!device || !origAddEventListener || hookedInputDevices.has(device)) {
      return;
    }
    hookedInputDevices.add(device);
    origAddEventListener.call(device, 'inputreport', function (event) {
      const bytes = toUint8Array(event.data);
      recordPacket(event.device || device, 'IN', event.reportId || 0, bytes, [
        '<HID InputReport Interrupt>',
      ]);
    });
  }

  function inspectDeviceCollections(device) {
    if (!device || inspectedDevices.has(device)) {
      return;
    }
    const collections = (device.collections || []).map((col) => ({
      usagePage: hex16(col.usagePage),
      usagePageDec: col.usagePage,
      usage: hex16(col.usage),
      usageDec: col.usage,
      type: col.type,
      inputReports: (col.inputReports || []).map((r) => hex8(r.reportId)),
      outputReports: (col.outputReports || []).map((r) => hex8(r.reportId)),
      featureReports: (col.featureReports || []).map((r) => hex8(r.reportId)),
    }));
    const meta = {
      productName: device.productName,
      vendorId: hex16(device.vendorId),
      vendorIdDec: device.vendorId,
      productId: hex16(device.productId),
      productIdDec: device.productId,
      collections,
    };
    inspectedDevices.set(device, meta);
    console.log(
      '%c[BKR1X-SNIFFER] Enumerated HIDDevice Descriptor:',
      'color:#9ece6a;font-weight:bold',
      meta
    );
    attachPassiveInputHook(device);
  }

  const origRequestDevice = navigator.hid.requestDevice.bind(navigator.hid);
  navigator.hid.requestDevice = async function (options) {
    const stack = captureCallStack(2);
    const rawFilters = (options && options.filters) || [];
    const formattedFilters = rawFilters.map((f) => ({
      vendorId: f.vendorId !== undefined ? `${hex16(f.vendorId)} (${f.vendorId})` : '*',
      productId: f.productId !== undefined ? `${hex16(f.productId)} (${f.productId})` : '*',
      usagePage: f.usagePage !== undefined ? `${hex16(f.usagePage)} (${f.usagePage})` : '*',
      usage: f.usage !== undefined ? `${hex16(f.usage)} (${f.usage})` : '*',
    }));

    pushBounded(
      filterLog,
      {
        timestamp: new Date().toISOString(),
        rawFilters,
        formattedFilters,
        stack,
      },
      MAX_CAPTURED_FILTERS
    );

    console.group('%c[BKR1X-SNIFFER] navigator.hid.requestDevice()', 'color:#e0af68;font-weight:bold');
    console.table(formattedFilters);
    console.log('Raw options:', options);
    console.log('CallStack:\n  ' + stack.join('\n  '));
    console.groupEnd();

    const devices = await origRequestDevice(options);
    (devices || []).forEach(inspectDeviceCollections);
    return devices;
  };

  const origGetDevices = navigator.hid.getDevices.bind(navigator.hid);
  navigator.hid.getDevices = async function (...args) {
    const devices = await origGetDevices(...args);
    (devices || []).forEach(inspectDeviceCollections);
    return devices;
  };

  if (hidProto) {
    const origOpen = hidProto.open;
    hidProto.open = async function (...args) {
      inspectDeviceCollections(this);
      console.log(
        `%c[BKR1X-SNIFFER] HIDDevice.open() -> ${this.productName} (${hex16(this.vendorId)}:${hex16(this.productId)})`,
        'color:#9ece6a;font-weight:bold'
      );
      return origOpen.apply(this, args);
    };

    const origSendReport = hidProto.sendReport;
    hidProto.sendReport = async function (reportId, data) {
      attachPassiveInputHook(this);
      recordPacket(this, 'OUT', reportId, toUint8Array(data), captureCallStack(2));
      return origSendReport.call(this, reportId, data);
    };

    const origSendFeatureReport = hidProto.sendFeatureReport;
    hidProto.sendFeatureReport = async function (reportId, data) {
      attachPassiveInputHook(this);
      recordPacket(this, 'FEATURE_OUT', reportId, toUint8Array(data), captureCallStack(2));
      return origSendFeatureReport.call(this, reportId, data);
    };

    const origReceiveFeatureReport = hidProto.receiveFeatureReport;
    hidProto.receiveFeatureReport = async function (reportId) {
      attachPassiveInputHook(this);
      const stack = captureCallStack(2);
      const view = await origReceiveFeatureReport.call(this, reportId);
      recordPacket(this, 'FEATURE_IN', reportId, toUint8Array(view), stack);
      return view;
    };

    hidProto.addEventListener = function (type, listener, options) {
      if (type === 'inputreport') {
        attachPassiveInputHook(this);
      }
      return origAddEventListener.call(this, type, listener, options);
    };

    const oninputDesc = Object.getOwnPropertyDescriptor(hidProto, 'oninputreport');
    if (oninputDesc && oninputDesc.set) {
      Object.defineProperty(hidProto, 'oninputreport', {
        configurable: true,
        enumerable: oninputDesc.enumerable,
        get() {
          return oninputDesc.get.call(this);
        },
        set(handler) {
          attachPassiveInputHook(this);
          return oninputDesc.set.call(this, handler);
        },
      });
    }
  }

  window.__BKR1X_SNIFFER__ = {
    filters: filterLog,
    packets: packetLog,
    clear() {
      packetLog.length = 0;
      console.info('[BKR1X-SNIFFER] Cleared packet capture buffer.');
    },
    summary() {
      console.table(
        packetLog.map((r) => ({
          seq: r.seq,
          dir: r.direction,
          vidPid: `${r.vendorId}:${r.productId}`,
          rpt: r.reportId,
          len: r.length,
          note: r.annotation,
          hexPrefix: r.hex.slice(0, 47),
        }))
      );
    },
    exportJson() {
      const payload = JSON.stringify({ filters: filterLog, packets: packetLog }, null, 2);
      const url = URL.createObjectURL(new Blob([payload], { type: 'application/json' }));
      const anchor = document.createElement('a');
      anchor.href = url;
      anchor.download = `bkr1x-hid-capture-${Date.now()}.json`;
      anchor.click();
      URL.revokeObjectURL(url);
      return payload;
    },
  };

  console.log(
    '%c[BKR1X-SNIFFER] Armed! Connect your BK-R1X mouse; run __BKR1X_SNIFFER__.summary() or __BKR1X_SNIFFER__.exportJson() anytime.',
    'background:#1a1b26;color:#7aa2f7;padding:4px 8px;border-radius:4px;font-weight:bold'
  );
})();
