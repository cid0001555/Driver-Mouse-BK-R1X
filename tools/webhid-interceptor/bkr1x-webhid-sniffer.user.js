// ==UserScript==
// @name         Fire Phoenix BK-R1X WebHID Protocol Interceptor & Dumper
// @namespace    https://github.com/driver-mouse-bkr1x
// @version      1.0.0
// @description  Hooks navigator.hid and HIDDevice before page scripts load to dump VID/PID filters, OUT/FEATURE reports, IN telemetry, and UI call stacks on bk-r1x.yjx2012.com.
// @author       BK-R1X Reverse Engineering Toolkit
// @match        https://bk-r1x.yjx2012.com/*
// @match        https://*.yjx2012.com/*
// @run-at       document-start
// @grant        none
// ==/UserScript==

(function () {
  'use strict';

  if (window.__BKR1X_SNIFFER_INSTALLED__) {
    console.warn('[BKR1X-SNIFFER] Already installed.');
    return;
  }
  window.__BKR1X_SNIFFER_INSTALLED__ = true;

  const LOG_RECORDS = [];
  const FILTER_RECORDS = [];
  const DEVICE_META = new WeakMap();

  // Known YJX2012 / BK-R1X opcodes for live inline annotation
  const TX_OPCODES = {
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
  };

  const RX_OPCODES = {
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
    0xfa: 'ASYNC_TELEMETRY_PUSH (Battery / DPI / Rate)',
  };

  function hex8(n) {
    return '0x' + (n & 0xff).toString(16).padStart(2, '0').toUpperCase();
  }

  function hex16(n) {
    return '0x' + (n & 0xffff).toString(16).padStart(4, '0').toUpperCase();
  }

  function toUint8Array(dataSource) {
    if (dataSource instanceof Uint8Array) {
      return new Uint8Array(dataSource.buffer, dataSource.byteOffset, dataSource.byteLength);
    }
    if (ArrayBuffer.isView(dataSource)) {
      return new Uint8Array(dataSource.buffer, dataSource.byteOffset, dataSource.byteLength);
    }
    if (dataSource instanceof ArrayBuffer) {
      return new Uint8Array(dataSource);
    }
    return new Uint8Array(0);
  }

  function formatHexBytes(bytes) {
    return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join(' ');
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

  function decodeAnnotation(direction, reportId, bytes) {
    if (bytes.length === 0) return 'EMPTY';
    if (direction === 'OUT' || direction === 'FEATURE_OUT') {
      if (bytes[0] === 0x55 && bytes.length >= 2) {
        const op = bytes[1];
        return `TX_MAGIC(0x55) cmd=${hex8(op)} [${TX_OPCODES[op] || 'UNKNOWN_CMD'}]`;
      }
      if (reportId === 0x06) {
        const op = bytes[0];
        return `REPORT_ID(0x06) cmd=${hex8(op)} [${TX_OPCODES[op] || 'MACRO_OP'}]`;
      }
    } else if (direction === 'IN' || direction === 'FEATURE_IN') {
      if (bytes[0] === 0xaa && bytes.length >= 2) {
        const op = bytes[1];
        let extra = '';
        if (op === 0xfa && bytes.length >= 11) {
          if (bytes[8] === 0xd0) {
            extra = ` battery=${bytes[9]}% charging=${bytes[10]}`;
          } else {
            extra = ` dpiStage=${bytes[9]} rateIdx=${bytes[10]}`;
          }
        } else if (op === 0x30 && bytes.length >= 10) {
          extra = ` battery=${bytes[8]}% charging=${bytes[9]}`;
        } else if (op === 0xed && bytes.length >= 9) {
          extra = ` online=${bytes[8] === 2}`;
        }
        return `RX_MAGIC(0xAA) op=${hex8(op)} [${RX_OPCODES[op] || 'UNKNOWN_ACK'}]${extra}`;
      }
    }
    return 'RAW_FRAME';
  }

  function recordPacket(device, direction, reportId, bytes, stack) {
    const now = new Date();
    const tsIso = now.toISOString();
    const perfMs = Number(performance.now().toFixed(3));
    const annotation = decodeAnnotation(direction, reportId, bytes);
    const hex = formatHexBytes(bytes);

    const entry = {
      seq: LOG_RECORDS.length + 1,
      timestamp: tsIso,
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
    LOG_RECORDS.push(entry);

    const badgeColor =
      direction === 'OUT'
        ? 'color:#ff9e64;font-weight:bold'
        : direction.startsWith('FEATURE')
        ? 'color:#bb9af7;font-weight:bold'
        : 'color:#7dcfff;font-weight:bold';

    console.groupCollapsed(
      `%c[BKR1X ${direction}]%c #${entry.seq} @ ${tsIso} | rpt=${entry.reportId} (${bytes.length}B) | ${annotation}`,
      badgeColor,
      'color:#c0caf5;font-weight:normal'
    );
    console.log('Device   :', `${entry.productName} (${entry.vendorId}:${entry.productId})`);
    console.log('Hex Dump :', hex);
    console.log('Bytes    :', Array.from(bytes));
    if (stack && stack.length > 0) {
      console.log('CallStack:\n  ' + stack.join('\n  '));
    }
    console.groupEnd();
  }

  function inspectDeviceCollections(device) {
    if (!device || DEVICE_META.has(device)) return;
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
    DEVICE_META.set(device, meta);
    console.log('%c[BKR1X-SNIFFER] Enumerated HIDDevice Descriptor:', 'color:#9ece6a;font-weight:bold', meta);
    attachPassiveInputHook(device);
  }

  // Ensure every opened HIDDevice logs incoming 'inputreport' frames even if the app uses
  // either addEventListener('inputreport', ...) OR device.oninputreport = fn.
  const HOOKED_DEVICES = new WeakSet();
  function attachPassiveInputHook(device) {
    if (!device || HOOKED_DEVICES.has(device)) return;
    HOOKED_DEVICES.add(device);
    origAddEventListener.call(device, 'inputreport', function (event) {
      const bytes = toUint8Array(event.data);
      recordPacket(event.device || device, 'IN', event.reportId || 0, bytes, ['<HID InputReport Interrupt>']);
    });
  }

  if (!('hid' in navigator)) {
    console.warn('[BKR1X-SNIFFER] navigator.hid is unavailable in this browser/context (use Chromium over HTTPS).');
    return;
  }

  // 1. Monkey-patch navigator.hid.requestDevice & navigator.hid.getDevices
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

    FILTER_RECORDS.push({
      timestamp: new Date().toISOString(),
      rawFilters,
      formattedFilters,
      stack,
    });

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

  // 2. Monkey-patch HIDDevice.prototype.open, sendReport, sendFeatureReport, receiveFeatureReport
  const proto = window.HIDDevice && window.HIDDevice.prototype;
  const origAddEventListener = proto.addEventListener;

  if (proto) {
    const origOpen = proto.open;
    proto.open = async function (...args) {
      inspectDeviceCollections(this);
      console.log(
        `%c[BKR1X-SNIFFER] HIDDevice.open() -> ${this.productName} (${hex16(this.vendorId)}:${hex16(this.productId)})`,
        'color:#9ece6a;font-weight:bold'
      );
      return origOpen.apply(this, args);
    };

    const origSendReport = proto.sendReport;
    proto.sendReport = async function (reportId, data) {
      attachPassiveInputHook(this);
      const bytes = toUint8Array(data);
      const stack = captureCallStack(2);
      recordPacket(this, 'OUT', reportId, bytes, stack);
      return origSendReport.call(this, reportId, data);
    };

    const origSendFeatureReport = proto.sendFeatureReport;
    proto.sendFeatureReport = async function (reportId, data) {
      attachPassiveInputHook(this);
      const bytes = toUint8Array(data);
      const stack = captureCallStack(2);
      recordPacket(this, 'FEATURE_OUT', reportId, bytes, stack);
      return origSendFeatureReport.call(this, reportId, data);
    };

    const origReceiveFeatureReport = proto.receiveFeatureReport;
    proto.receiveFeatureReport = async function (reportId) {
      attachPassiveInputHook(this);
      const stack = captureCallStack(2);
      const view = await origReceiveFeatureReport.call(this, reportId);
      const bytes = toUint8Array(view);
      recordPacket(this, 'FEATURE_IN', reportId, bytes, stack);
      return view;
    };

    // 3. Intercept addEventListener('inputreport', ...) AND oninputreport setter
    proto.addEventListener = function (type, listener, options) {
      if (type === 'inputreport') {
        attachPassiveInputHook(this);
        console.debug('[BKR1X-SNIFFER] Page registered addEventListener("inputreport")');
      }
      return origAddEventListener.call(this, type, listener, options);
    };

    const oninputDesc = Object.getOwnPropertyDescriptor(proto, 'oninputreport');
    if (oninputDesc && oninputDesc.set) {
      Object.defineProperty(proto, 'oninputreport', {
        configurable: true,
        enumerable: oninputDesc.enumerable,
        get: function () {
          return oninputDesc.get.call(this);
        },
        set: function (handler) {
          attachPassiveInputHook(this);
          console.debug('[BKR1X-SNIFFER] Page assigned HIDDevice.oninputreport setter');
          return oninputDesc.set.call(this, handler);
        },
      });
    }
  }

  // 4. Expose interactive export API on window.__BKR1X_SNIFFER__
  window.__BKR1X_SNIFFER__ = {
    filters: FILTER_RECORDS,
    packets: LOG_RECORDS,
    clear() {
      LOG_RECORDS.length = 0;
      console.info('[BKR1X-SNIFFER] Cleared packet capture buffer.');
    },
    summary() {
      console.table(
        LOG_RECORDS.map((r) => ({
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
      const blob = JSON.stringify({ filters: FILTER_RECORDS, packets: LOG_RECORDS }, null, 2);
      const url = URL.createObjectURL(new Blob([blob], { type: 'application/json' }));
      const a = document.createElement('a');
      a.href = url;
      a.download = `bkr1x-hid-capture-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
      return blob;
    },
  };

  console.log(
    '%c[BKR1X-SNIFFER] Armed! Connect your BK-R1X mouse; run __BKR1X_SNIFFER__.summary() or __BKR1X_SNIFFER__.exportJson() anytime.',
    'background:#1a1b26;color:#7aa2f7;padding:4px 8px;border-radius:4px;font-weight:bold'
  );
})();
