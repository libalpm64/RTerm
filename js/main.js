import {
  sessions, activeId, commandHistory,
  setFontSize, setCursorStyle, setCursorBlink, setScrollback,
  setSemanticHlEnabled
} from './state.js';
import { setupRtermApi } from './ipc.js';
import { updateKeywords } from './highlighting.js';
import {
  newTerminal, updateStatus, updateTheme, renderHistory, fitAll
} from './terminal.js';
import { renderSavedSessions, initSessions, initSavedSessions } from './sessions.js';
import { setupSaveDialog } from './sftp.js';
import { loadLocalDir } from './filer.js';
import { setupEventListeners, initSettingsTabs } from './ui.js';
import { byId, setDisplay, setToggle, setValue } from './dom.js';

document.addEventListener('DOMContentLoaded', async () => {
  setupRtermApi();
  setupSaveDialog();
  setupEventListeners();
  initSettingsTabs();
  await initSavedSessions();
  initSessions();
  renderSavedSessions();
  updateStatus();
  setInterval(() => updateStatus(activeId), 1000);
  setTimeout(() => loadLocalDir('~'), 200);
  if (sessions.size === 0) {
    newTerminal();
  }
  const _termEl = byId('terminals');
  if (_termEl) {
    const _termObs = new ResizeObserver(() => {
      fitAll();
    });
    _termObs.observe(_termEl);
  }
  window.addEventListener('resize', () => {
    fitAll();
  });
  if (window.rterm) {
    await loadBackendSettings();
  }
});

async function loadBackendSettings() {
  const vaultEnabled = await window.rterm.loadSetting('vault_enabled');
  const vaultToggle = byId('setting-vault');
  if (vaultToggle && vaultEnabled?.result === '1') {
    vaultToggle.classList.add('on');
    const exists = await window.rterm.vaultExists();
    if (exists?.result) {
      setDisplay(byId('lock-screen'), 'flex');
      byId('lock-pass')?.focus();
    }
  }

  const fs = await window.rterm.loadSetting('font_size');
  if (fs?.result) {
    setFontSize(parseInt(fs.result));
    setValue(byId('setting-font'), parseInt(fs.result));
  }

  const cs = await window.rterm.loadSetting('cursor_style');
  if (cs?.result) {
    setCursorStyle(cs.result);
    setValue(byId('setting-cursor'), cs.result);
  }

  const cb = await window.rterm.loadSetting('cursor_blink');
  if (cb?.result) {
    setCursorBlink(cb.result === '1');
    setToggle(byId('setting-cursor-blink'), cb.result === '1');
  }

  const sb = await window.rterm.loadSetting('scrollback');
  if (sb?.result) {
    setScrollback(parseInt(sb.result));
    setValue(byId('setting-scrollback'), parseInt(sb.result));
  }

  const theme = await window.rterm.loadSetting('theme');
  if (theme?.result) {
    setValue(byId('setting-theme'), theme.result);
    updateTheme(theme.result);
  }

  const bell = await window.rterm.loadSetting('bell_enabled');
  if (bell?.result) {
    setToggle(byId('setting-bell'), bell.result === '1');
  }

  const copySelect = await window.rterm.loadSetting('copy_on_select');
  if (copySelect?.result) {
    setToggle(byId('setting-copy-select'), copySelect.result === '1');
  }

  const sshPort = await window.rterm.loadSetting('ssh_default_port');
  if (sshPort?.result) setValue(byId('setting-ssh-port'), sshPort.result);

  const keepalive = await window.rterm.loadSetting('ssh_keepalive');
  if (keepalive?.result) setValue(byId('setting-keepalive'), keepalive.result);

  const compression = await window.rterm.loadSetting('ssh_compression');
  if (compression?.result) {
    setToggle(byId('setting-compression'), compression.result === '1');
  }

  const baud = await window.rterm.loadSetting('serial_default_baud');
  if (baud?.result) setValue(byId('setting-serial-baud'), baud.result);

  const bits = await window.rterm.loadSetting('serial_default_bits');
  if (bits?.result) setValue(byId('setting-serial-bits'), bits.result);

  const lockTimeout = await window.rterm.loadSetting('lock_timeout');
  if (lockTimeout?.result) setValue(byId('setting-lock-timeout'), lockTimeout.result);

  const confirmExit = await window.rterm.loadSetting('confirm_exit');
  if (confirmExit?.result) {
    setToggle(byId('setting-confirm-exit'), confirmExit.result === '1');
  }

  const clearClose = await window.rterm.loadSetting('clear_on_close');
  if (clearClose?.result) {
    setToggle(byId('setting-clear-close'), clearClose.result === '1');
  }

  const sftpConcurrent = await window.rterm.loadSetting('sftp_concurrent');
  if (sftpConcurrent?.result) setValue(byId('setting-sftp-concurrent'), sftpConcurrent.result);

  const sftpSpeed = await window.rterm.loadSetting('sftp_max_speed');
  if (sftpSpeed?.result) setValue(byId('setting-sftp-speed'), sftpSpeed.result);

  const keywords = await window.rterm.loadSetting('highlight_keywords');
  if (keywords?.result) {
    setValue(byId('setting-keywords'), keywords.result);
    updateKeywords(keywords.result);
  }

  const semHl = await window.rterm.loadSetting('semantic_hl');
  if (semHl?.result) {
    setSemanticHlEnabled(semHl.result === '1');
    setToggle(byId('setting-semantic-hl'), semHl.result === '1');
  }

  const history = await window.rterm.loadSetting('command_history');
  if (history?.result) {
    try {
      const h = JSON.parse(history.result);
      if (Array.isArray(h)) {
        commandHistory.push(...h);
        renderHistory();
      }
    } catch (e) { }
  }
}
