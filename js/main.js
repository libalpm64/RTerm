import {
  sessions, activeId, fontSize, cursorStyle, cursorBlink, scrollback,
  semanticHlEnabled, commandHistory, savedSessions, savedSessionsDirty,
  setFontSize, setCursorStyle, setCursorBlink, setScrollback,
  setSemanticHlEnabled
} from './state.js';
import { _ipc, setupRtermApi } from './ipc.js';
import { updateKeywords } from './highlighting.js';
import {
  newTerminal, renderTabs, updateStatus, updateTheme, renderHistory,
  fitAll, showTerminalView
} from './terminal.js';
import { renderSavedSessions, initSessions, initSavedSessions } from './sessions.js';
import { setupSaveDialog } from './sftp.js';
import { loadLocalDir } from './filer.js';
import { setupEventListeners, closeMenusInternal, hideModalInternal, showModalInternal, initSettingsTabs } from './ui.js';

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
  const _termEl = document.getElementById('terminals');
  if (_termEl) {
    const _termObs = new ResizeObserver(() => {
      import('./terminal.js').then(mod => mod.fitAll());
    });
    _termObs.observe(_termEl);
  }
  window.addEventListener('resize', () => {
    import('./terminal.js').then(mod => mod.fitAll());
  });
  if (window.rterm) {
    await loadBackendSettings();
  }
});

async function loadBackendSettings() {
  const vaultEnabled = await window.rterm.loadSetting('vault_enabled');
  const vaultToggle = document.getElementById('setting-vault');
  if (vaultToggle && vaultEnabled?.result === '1') {
    vaultToggle.classList.add('on');
    const exists = await window.rterm.vaultExists();
    if (exists?.result) {
      document.getElementById('lock-screen').style.display = 'flex';
      document.getElementById('lock-pass').focus();
    }
  }

  const fs = await window.rterm.loadSetting('font_size');
  if (fs?.result) {
    setFontSize(parseInt(fs.result));
    document.getElementById('setting-font').value = parseInt(fs.result);
  }

  const cs = await window.rterm.loadSetting('cursor_style');
  if (cs?.result) {
    setCursorStyle(cs.result);
    document.getElementById('setting-cursor').value = cs.result;
  }

  const cb = await window.rterm.loadSetting('cursor_blink');
  if (cb?.result) {
    setCursorBlink(cb.result === '1');
    if (cb.result === '1') document.getElementById('setting-cursor-blink').classList.add('on');
    else document.getElementById('setting-cursor-blink').classList.remove('on');
  }

  const sb = await window.rterm.loadSetting('scrollback');
  if (sb?.result) {
    setScrollback(parseInt(sb.result));
    document.getElementById('setting-scrollback').value = parseInt(sb.result);
  }

  const theme = await window.rterm.loadSetting('theme');
  if (theme?.result) {
    document.getElementById('setting-theme').value = theme.result;
    updateTheme(theme.result);
  }

  const bell = await window.rterm.loadSetting('bell_enabled');
  if (bell?.result) {
    if (bell.result === '1') document.getElementById('setting-bell').classList.add('on');
    else document.getElementById('setting-bell').classList.remove('on');
  }

  const copySelect = await window.rterm.loadSetting('copy_on_select');
  if (copySelect?.result) {
    if (copySelect.result === '1') document.getElementById('setting-copy-select').classList.add('on');
    else document.getElementById('setting-copy-select').classList.remove('on');
  }

  const sshPort = await window.rterm.loadSetting('ssh_default_port');
  if (sshPort?.result) document.getElementById('setting-ssh-port').value = sshPort.result;

  const keepalive = await window.rterm.loadSetting('ssh_keepalive');
  if (keepalive?.result) document.getElementById('setting-keepalive').value = keepalive.result;

  const compression = await window.rterm.loadSetting('ssh_compression');
  if (compression?.result) {
    if (compression.result === '1') document.getElementById('setting-compression').classList.add('on');
    else document.getElementById('setting-compression').classList.remove('on');
  }

  const baud = await window.rterm.loadSetting('serial_default_baud');
  if (baud?.result) document.getElementById('setting-serial-baud').value = baud.result;

  const bits = await window.rterm.loadSetting('serial_default_bits');
  if (bits?.result) document.getElementById('setting-serial-bits').value = bits.result;

  const lockTimeout = await window.rterm.loadSetting('lock_timeout');
  if (lockTimeout?.result) document.getElementById('setting-lock-timeout').value = lockTimeout.result;

  const confirmExit = await window.rterm.loadSetting('confirm_exit');
  if (confirmExit?.result) {
    if (confirmExit.result === '1') document.getElementById('setting-confirm-exit').classList.add('on');
    else document.getElementById('setting-confirm-exit').classList.remove('on');
  }

  const clearClose = await window.rterm.loadSetting('clear_on_close');
  if (clearClose?.result) {
    if (clearClose.result === '1') document.getElementById('setting-clear-close').classList.add('on');
    else document.getElementById('setting-clear-close').classList.remove('on');
  }

  const sftpConcurrent = await window.rterm.loadSetting('sftp_concurrent');
  if (sftpConcurrent?.result) document.getElementById('setting-sftp-concurrent').value = sftpConcurrent.result;

  const sftpSpeed = await window.rterm.loadSetting('sftp_max_speed');
  if (sftpSpeed?.result) document.getElementById('setting-sftp-speed').value = sftpSpeed.result;

  const keywords = await window.rterm.loadSetting('highlight_keywords');
  if (keywords?.result) {
    document.getElementById('setting-keywords').value = keywords.result;
    updateKeywords(keywords.result);
  }

  const semHl = await window.rterm.loadSetting('semantic_hl');
  if (semHl?.result) {
    setSemanticHlEnabled(semHl.result === '1');
    if (semHl.result === '1') document.getElementById('setting-semantic-hl').classList.add('on');
    else document.getElementById('setting-semantic-hl').classList.remove('on');
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
