import {
  sessions, activeId, activeMenu, savedSessions, savedSessionsDirty,
  cursorBlink, semanticHlEnabled, _editingIndex, sftpSshId,
  setActiveMenu, setEditingIndex, setSavedSessionsDirty,
  setSemanticHlEnabled, setSftpInitialized, setActiveId
} from './state.js';
import {
  newTerminal, selectTab, showTerminal, showTerminalView, showKeysPanel,
  renderTabs, toggleSidebar, zoomIn, zoomOut, resetZoom,
  copySelection, pasteClipboard, selectAll, clearTerminal,
  showContextMenu, showQuickMenu, updateStatus,
  updateFontSize, updateScrollback, updateCursorStyle, updateCursorBlink,
  updateTheme, updateBell, updateCopySelect,
  closeTab, addHistoryItem, renderHistory, executeHistory,
  fitAll, doConnect
} from './terminal.js';
import { renderSavedSessions, addSession, editSession, deleteSession, connectSession, initSavedSessions } from './sessions.js';
import { loadSftpDir, setupSaveDialog, showSaveDialog, getCurrentSftpPath, setSftpSelectMode } from './sftp.js';
import { setupKeys } from './keys.js';
import { loadLocalDir } from './filer.js';
import { $, $$, byId, on, setDisplay, setText, showOnlyById } from './dom.js';

function ensureDlBar() {
  let bar = document.getElementById('dl-progress');
  if (!bar) {
    bar = document.createElement('div');
    bar.id = 'dl-progress';
    bar.style.cssText = 'position:fixed;bottom:30px;left:12px;min-width:280px;max-width:420px;background:var(--bg3);padding:10px 12px;z-index:9999;border:1px solid var(--border2);border-radius:8px;box-shadow:0 8px 24px rgba(0,0,0,.4);font-size:12px;display:none;';
    bar.innerHTML =
      '<div class="dl-label" style="margin-bottom:6px;color:var(--text);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;"></div>' +
      '<div class="dl-track" style="height:4px;border-radius:2px;background:var(--bg2);overflow:hidden;"><div class="dl-fill" style="height:100%;width:0%;background:var(--accent);border-radius:2px;transition:width .25s;"></div></div>';
    document.body.appendChild(bar);
  }
  return bar;
}

let _dlHideTimer = null;
window.__rterm_dlProgress = function (text, pct, opts) {
  const bar = ensureDlBar();
  const label = bar.querySelector('.dl-label');
  const fill = bar.querySelector('.dl-fill');
  if (_dlHideTimer) { clearTimeout(_dlHideTimer); _dlHideTimer = null; }
  bar.style.display = 'block';
  label.textContent = text || '';
  if (typeof pct === 'number' && isFinite(pct)) {
    fill.style.width = Math.max(0, Math.min(100, pct)) + '%';
  }
  const o = opts || {};
  const color = o.error ? 'var(--red)' : o.done ? 'var(--green)' : 'var(--accent)';
  fill.style.background = color;
  label.style.color = o.error ? 'var(--red)' : o.done ? 'var(--green)' : 'var(--text)';
  if (o.done || o.error) {
    _dlHideTimer = setTimeout(() => {
      bar.style.display = 'none';
      fill.style.width = '0%';
    }, o.error ? 6000 : 4000);
  }
};

export function closeMenusInternal() {
  $$('.dropdown').forEach(m => m.classList.remove('show'));
  $$('.menu-btn').forEach(b => b.classList.remove('active'));
  setActiveMenu(null);
}

export function hideModalInternal() {
  byId('modal-overlay')?.classList.remove('show');
}

export function showModalInternal(type) {
  closeMenusInternal();
  showOnlyById({
    'modal-connect': type === 'connect',
    'modal-settings': type === 'settings',
    'modal-about': type === 'about',
    'modal-keys': type === 'keys',
  });
  byId('modal-overlay')?.classList.add('show');

  if (type === 'connect') {
    setText(byId('modal-title'), 'New Session');
    resetConnectModal();
  } else if (type === 'settings') {
    setText(byId('modal-title'), 'Settings');
    initSettingsTabs();
  }
}

function resetConnectModal() {
  setEditingIndex(-1);
  setText(byId('modal-title'), 'New Session');
  setText(byId('modal-connect-btn'), 'Connect');
  $$('#modal-connect .modal-tab').forEach(t => t.classList.remove('active'));
  $('#modal-connect .modal-tab[data-type="local"]')?.classList.add('active');
  setConnectionFields('local');

  $$('#modal-connect .toggle').forEach(t => t.classList.add('on'));
}

export function initSettingsTabs() {
  $$('#modal-settings .modal-tab').forEach(t => t.classList.remove('active'));
  $('#modal-settings .modal-tab[data-settings="terminal"]')?.classList.add('active');
  $$('.settings-content').forEach(s => s.classList.remove('active'));
  byId('settings-terminal')?.classList.add('active');
}

function setConnectionFields(type) {
  setDisplay(byId('local-fields'), type === 'local' ? 'block' : 'none');
  setDisplay(byId('ssh-fields'), type === 'ssh' ? 'block' : 'none');
  setDisplay(byId('telnet-fields'), type === 'telnet' ? 'block' : 'none');
  setDisplay(byId('serial-fields'), type === 'serial' ? 'block' : 'none');
}

function setActiveWithin(items, activeItem) {
  items.forEach(item => item.classList.toggle('active', item === activeItem));
}

function isSftpPanelActive() {
  return document.querySelector('.sidebar-tab.active')?.dataset?.panel === 'sftp';
}

export function setupEventListeners() {

  $$('.menu-btn').forEach(btn => {
    on(btn, 'click', (e) => {
      e.stopPropagation();
      const menu = $('.dropdown', btn);
      if (activeMenu === menu) {
        closeMenusInternal();
      } else {
        closeMenusInternal();
        menu.classList.add('show');
        btn.classList.add('active');
        setActiveMenu(menu);
      }
    });
  });

  on(document, 'click', (e) => {
    if (!e.target.closest('.menu-btn') && !e.target.closest('.dropdown')) {
      closeMenusInternal();
    }
  });

  on(byId('modal-close-btn'), 'click', hideModalInternal);
  on(byId('modal-overlay'), 'click', (e) => {
    if (e.target === byId('modal-overlay')) hideModalInternal();
  });

  const connectTabs = $$('#modal-connect .modal-tab');
  connectTabs.forEach(tab => {
    on(tab, 'click', () => {
      setActiveWithin(connectTabs, tab);
      const type = tab.dataset.type;
      setConnectionFields(type);
    });
  });

  // SSH Auth method toggle
  on(byId('ssh-auth-method'), 'change', async (e) => {
    const method = e.target.value;
    setDisplay(byId('ssh-password-fields'), method === 'password' ? '' : 'none');
    const keyFields = byId('ssh-key-fields');
    setDisplay(keyFields, method === 'key' ? '' : 'none');
    if (method === 'key') {
      const sel = byId('ssh-key-select');
      const prev = sel.value;
      sel.innerHTML = '<option value="">Select a key...</option>';
      const vaultKeys = window.__rterm_vault_keys || [];
      vaultKeys.forEach(k => {
        const opt = document.createElement('option');
        opt.value = k.name;
        opt.textContent = k.name + ' (vault)';
        opt.style.color = 'var(--accent)';
        sel.appendChild(opt);
      });
      if (window.rterm) {
        try {
          const keys = await window.rterm.listKeys();
          if (keys.success && keys.result) {
            keys.result.forEach(k => {
              const opt = document.createElement('option');
              opt.value = k.path;
              opt.textContent = k.name + (k.type !== 'unknown' ? ' (' + k.type + ')' : '');
              sel.appendChild(opt);
            });
            if (prev) sel.value = prev;
          }
        } catch (e) { }
      }
    }
  });

  const settingsTabs = $$('#modal-settings .modal-tab');
  settingsTabs.forEach(tab => {
    on(tab, 'click', () => {
      setActiveWithin(settingsTabs, tab);
      const section = tab.dataset.settings;
      $$('.settings-content').forEach(s => s.classList.remove('active'));
      byId('settings-' + section)?.classList.add('active');
    });
  });

  // Toggle switches
  $$('.toggle').forEach(toggle => {
    on(toggle, 'click', () => toggle.classList.toggle('on'));
  });

  // Generate SSH Key button (in connect modal)
  document.getElementById('gen-key-btn').addEventListener('click', () => {
    const type = document.getElementById('gen-key-type').value;
    const name = document.getElementById('gen-key-name').value || 'id_' + type;
    alert('SSH Key generation: ' + type + ' - ' + name + '\n(Feature coming soon)');
  });

  document.querySelectorAll('.sidebar-tab').forEach(tab => {
    tab.addEventListener('click', async () => {
      document.querySelectorAll('.sidebar-tab').forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      const panel = tab.dataset.panel;
      document.querySelectorAll('.sidebar-panel').forEach(p => p.classList.remove('active'));
      document.getElementById('panel-' + panel).classList.add('active');

      if (panel === 'sftp') setTimeout(() => loadSftpDir(getCurrentSftpPath() || '/'), 50);
      if (panel === 'local') setTimeout(() => loadLocalDir('~'), 50);
    });
  });

  window.addEventListener('rterm:ssh-connected', () => {
    if (isSftpPanelActive()) {
      setSftpInitialized(false);
      loadSftpDir(getCurrentSftpPath() || '/');
    }
  });

  // SFTP menus
  document.getElementById('sftp-list').addEventListener('contextmenu', (e) => {
    const sftpSshId = window.__rterm_sftpSshId;
    if (!sftpSshId) return;
    const fileItem = e.target.closest('.filer-item');
    if (fileItem && fileItem.oncontextmenu) return;
    e.preventDefault();
    e.stopPropagation();
    const old = document.getElementById('sftp-ctx');
    if (old) old.remove();
    const menu = document.createElement('div');
    menu.id = 'sftp-ctx';
    menu.style.cssText = 'position:fixed;z-index:9999;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;box-shadow:0 8px 24px rgba(0,0,0,.5);min-width:160px;font-size:12px;';
    menu.innerHTML = '<div class="dropdown-item" id="sctx-ul">Upload File</div><div class="dropdown-item" id="sctx-sm">Select Mode</div>';
    document.body.appendChild(menu);
    menu.style.left = Math.max(4, Math.min(e.clientX, window.innerWidth - 188)) + 'px';
    menu.style.top = Math.max(4, Math.min(e.clientY, window.innerHeight - 88)) + 'px';
    document.getElementById('sctx-ul').onclick = async () => {
      menu.remove();
      const sess = Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined);
      if (!sess) { alert('No active SSH session'); return; }
      const localFile = await showSaveDialog('', 'file');
      if (!localFile) return;
      const filename = localFile.split('/').pop() || 'upload.bin';
      const remoteDir = getCurrentSftpPath() || '/';
      const remotePath = (remoteDir === '/' ? '' : remoteDir) + '/' + filename;
      const result = await window.rterm.sftpUpload(sess.sshId, localFile, remotePath);
      if (!result?.success) alert('Upload failed: ' + (result?.error || 'unknown'));
    };
    document.getElementById('sctx-sm').onclick = () => {
      menu.remove();
      setSftpSelectMode(true);
      loadSftpDir(getCurrentSftpPath() || '/');
    };
    setTimeout(() => {
      document.addEventListener('click', () => { const m = document.getElementById('sftp-ctx'); if (m) m.remove(); }, { once: true });
    }, 10);
  });

  document.getElementById('sftp-refresh').onclick = () => {
    setSftpInitialized(false);
    if (sftpSshId) loadSftpDir('/');
  };

  document.getElementById('sftp-upload').onclick = async () => {
    const sess = Array.from(sessions.values()).find(s => s.type === 'ssh' && s.sshId !== null && s.sshId !== undefined);
    if (!sess) { alert('No active SSH session'); return; }
    const localFile = await showSaveDialog('', 'file');
    if (!localFile) return;

    const filename = localFile.split('/').pop() || 'upload.bin';
    const remoteDir = getCurrentSftpPath() || '/';
    const remotePath = (remoteDir === '/' ? '' : remoteDir) + '/' + filename;

    window.__rterm_dlProgress(`Starting upload of ${filename}...`, 0);

    const result = await window.rterm.sftpUpload(sess.sshId, localFile, remotePath);
    if (!result?.success) {
      window.__rterm_dlProgress('Upload failed: ' + (result?.error || 'unknown'), 100, { error: true });
    }
  };

  // Menubar Actions
  document.getElementById('action-new').onclick = () => { closeMenusInternal(); newTerminal(); };
  document.getElementById('action-connect').onclick = () => showModalInternal('connect');
  document.getElementById('action-settings').onclick = () => showModalInternal('settings');
  document.getElementById('action-exit').onclick = () => window.close();
  document.getElementById('action-copy').onclick = copySelection;
  document.getElementById('action-paste').onclick = pasteClipboard;
  document.getElementById('action-selectall').onclick = selectAll;
  document.getElementById('action-clear').onclick = clearTerminal;
  document.getElementById('action-toggle-filer').onclick = () => toggleSidebar('left');
  document.getElementById('action-toggle-sessions').onclick = () => toggleSidebar('right');
  document.getElementById('action-zoomin').onclick = zoomIn;
  document.getElementById('action-zoomout').onclick = zoomOut;
  document.getElementById('action-zoomreset').onclick = resetZoom;
  document.getElementById('action-about').onclick = () => showModalInternal('about');
  document.getElementById('action-docs').onclick = () => window.open('https://github.com', '_blank');
  document.getElementById('action-issue').onclick = () => window.open('https://github.com', '_blank');

  let keysTabId = 'keys-' + Date.now();
  let keysTabOpen = false;

  document.getElementById('action-keys-open').onclick = () => {
    closeMenusInternal();
    if (!keysTabOpen) {
      keysTabOpen = true;
      sessions.set(keysTabId, { name: 'SSH Keys', type: 'keys', term: null });
      setActiveId(keysTabId);
      renderTabs();
      showKeysPanel();
    } else {
      selectTab(keysTabId);
      showKeysPanel();
    }
  };

  const agentMenuItem = document.getElementById('action-keys-agent');
  if (agentMenuItem) agentMenuItem.remove();

  // Context menus
  document.getElementById('ctx-copy').onclick = copySelection;
  document.getElementById('ctx-paste').onclick = pasteClipboard;
  document.getElementById('ctx-clear').onclick = clearTerminal;
  document.getElementById('ctx-zoom').onclick = resetZoom;

  document.addEventListener('click', () => {
    document.getElementById('context-menu').classList.remove('show');
  });

  // Toolbar buttons
  document.getElementById('tab-add-btn').onclick = () => { closeMenusInternal(); newTerminal(); };
  document.getElementById('empty-new-btn').onclick = () => { closeMenusInternal(); newTerminal(); };
  document.getElementById('empty-connect-btn').onclick = () => showModalInternal('connect');
  document.getElementById('new-session-btn').onclick = () => { closeMenusInternal(); newTerminal(); };
  document.getElementById('menu-btn').onclick = showQuickMenu;
  document.getElementById('zoom-in-btn').onclick = zoomIn;
  document.getElementById('about-btn').onclick = () => showModalInternal('about');

  document.getElementById('rs-add-session').onclick = () => showModalInternal('connect');
  document.getElementById('rs-clear-history').onclick = () => {
    commandHistory.length = 0;
    renderHistory();
    window.rterm?.saveSetting('command_history', '[]');
  };

  // Session search filter
  const searchInput = document.getElementById('session-search');
  if (searchInput) {
    searchInput.addEventListener('input', function () {
      const q = this.value.toLowerCase();
      document.querySelectorAll('.session-item').forEach(el => {
        el.style.display = el.textContent.toLowerCase().includes(q) ? '' : 'none';
      });
    });
  }

  // Resize sidebars
  ['resize-left', 'resize-right'].forEach(id => {
    const bar = document.getElementById(id);
    if (!bar) return;
    let startX, startW;
    bar.addEventListener('mousedown', (e) => {
      e.preventDefault();
      const target = id === 'resize-left' ? document.getElementById('sidebar') : document.getElementById('right-sidebar');
      startX = e.clientX;
      startW = target.offsetWidth;
      bar.classList.add('active');
      const onMove = (ev) => {
        const diff = ev.clientX - startX;
        const newW = id === 'resize-left' ? Math.max(60, Math.min(500, startW + diff)) : Math.max(60, Math.min(500, startW - diff));
        requestAnimationFrame(() => { target.style.width = newW + 'px'; });
      };
      const onUp = () => { bar.classList.remove('active'); document.removeEventListener('mousemove', onMove); document.removeEventListener('mouseup', onUp); };
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
    });
  });

  // Modal buttons
  document.getElementById('modal-cancel-btn').onclick = hideModalInternal;
  document.getElementById('modal-connect-btn').onclick = () => { hideModalInternal(); doConnect(); };
  document.getElementById('settings-done-btn').onclick = saveAllSettings;
  document.getElementById('about-close-btn').onclick = hideModalInternal;

  // Settings UI
  document.getElementById('setting-font').onchange = (e) => updateFontSize(e.target.value);
  document.getElementById('setting-cursor').onchange = (e) => updateCursorStyle(e.target.value);
  document.getElementById('setting-scrollback').onchange = (e) => updateScrollback(e.target.value);
  document.getElementById('setting-cursor-blink').addEventListener('click', function () {
    this.classList.toggle('on');
    updateCursorBlink(this.classList.contains('on'));
  });
  document.getElementById('setting-semantic-hl').addEventListener('click', function () {
    this.classList.toggle('on');
    setSemanticHlEnabled(this.classList.contains('on'));
    window.rterm?.saveSetting('semantic_hl', this.classList.contains('on') ? '1' : '0');
  });
  document.getElementById('setting-theme').onchange = (e) => updateTheme(e.target.value);
  document.getElementById('setting-bell').onclick = function () {
    this.classList.toggle('on');
    updateBell(this.classList.contains('on'));
  };
  document.getElementById('setting-copy-select').onclick = function () {
    this.classList.toggle('on');
    updateCopySelect(this.classList.contains('on'));
  };

  // Tab bar click delegation
  document.getElementById('tabbar').onclick = (e) => {
    const closeBtn = e.target.closest('.tab-close');
    if (closeBtn) {
      const id = closeBtn.dataset.id;
      if (id) closeTab(id);
    }
  };

  // Keyboard shortcuts
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hideModalInternal();
    if (e.ctrlKey && e.key === 'n') { e.preventDefault(); closeMenusInternal(); newTerminal(); }
    if (e.ctrlKey && e.key === 'k') { e.preventDefault(); showModalInternal('connect'); }
    if (e.ctrlKey && e.key === 'w') { e.preventDefault(); if (activeId) closeTab(activeId); }
    if (e.ctrlKey && (e.key === '=' || e.key === '+')) { e.preventDefault(); zoomIn(); }
    if (e.ctrlKey && e.key === '-') { e.preventDefault(); zoomOut(); }
    if (e.ctrlKey && e.key === '0') { e.preventDefault(); resetZoom(); }
  });

  // Local file browser
  document.getElementById('filer-refresh').onclick = () => setTimeout(() => loadLocalDir(localPath), 100);
  document.getElementById('filer-toggle').onclick = () => toggleSidebar('left');

  // Vault handlers
  setupVaultHandlers();

  // Lock screen
  setupLockScreen();

  // SSH Keys toolbars and modals
  setupKeys();
}

async function saveAllSettings() {
  const vaultToggle = document.getElementById('setting-vault');
  const vaultPass = document.getElementById('setting-vault-pass');
  const isOn = vaultToggle?.classList.contains('on');

  await window.rterm.saveSetting('vault_enabled', isOn ? '1' : '0');

  if (isOn && vaultPass?.value) {
    const pass = vaultPass.value;
    await window.rterm.saveSessions(savedSessions.map(s => ({
      host: s.host, port: s.port, user: s.user,
      password: s.password, key_path: s.key_path, name: s.name
    })), pass, window.__rterm_vault_keys || []);
    window.__rterm_vault_pass = pass;
  } else if (!isOn) {
    await window.rterm.deleteVault();
    window.__rterm_vault_pass = '';
  }

  const sshPort = document.getElementById('setting-ssh-port')?.value;
  if (sshPort) await window.rterm.saveSetting('ssh_default_port', sshPort);

  const keepalive = document.getElementById('setting-keepalive')?.value;
  if (keepalive) await window.rterm.saveSetting('ssh_keepalive', keepalive);

  const compression = document.getElementById('setting-compression')?.classList.contains('on');
  await window.rterm.saveSetting('ssh_compression', compression ? '1' : '0');

  const baud = document.getElementById('setting-serial-baud')?.value;
  if (baud) await window.rterm.saveSetting('serial_default_baud', baud);

  const bits = document.getElementById('setting-serial-bits')?.value;
  if (bits) await window.rterm.saveSetting('serial_default_bits', bits);

  const lockTimeout = document.getElementById('setting-lock-timeout')?.value;
  if (lockTimeout) await window.rterm.saveSetting('lock_timeout', lockTimeout);

  const confirmExit = document.getElementById('setting-confirm-exit')?.classList.contains('on');
  await window.rterm.saveSetting('confirm_exit', confirmExit ? '1' : '0');

  const clearClose = document.getElementById('setting-clear-close')?.classList.contains('on');
  await window.rterm.saveSetting('clear_on_close', clearClose ? '1' : '0');

  const sftpConcurrent = document.getElementById('setting-sftp-concurrent')?.value;
  if (sftpConcurrent) await window.rterm.saveSetting('sftp_concurrent', sftpConcurrent);

  const sftpSpeed = document.getElementById('setting-sftp-speed')?.value;
  if (sftpSpeed) await window.rterm.saveSetting('sftp_max_speed', sftpSpeed);

  const keywords = document.getElementById('setting-keywords')?.value;
  if (keywords !== undefined) {
    import('./highlighting.js').then(mod => mod.updateKeywords(keywords));
  }

  hideModalInternal();
}

function setupVaultHandlers() {
  // No additional handlers needed beyond settings save
}

function setupLockScreen() {
  document.getElementById('lock-unlock-btn').onclick = async () => {
    const pass = document.getElementById('lock-pass').value;
    const errEl = document.getElementById('lock-error');
    const result = await window.rterm.loadSessions(pass);
    if (result.success) {
      window.__rterm_vault_pass = pass;
      window.__rterm_vault_keys = result.keys || [];
      document.getElementById('lock-screen').style.display = 'none';
      if (result.result) {
        savedSessions.length = 0;
        result.result.forEach(s => savedSessions.push({ ...s, type: 'ssh', name: s.name || s.user + '@' + s.host }));
        setSavedSessionsDirty(true);
        renderSavedSessions();
      }
    } else {
      errEl.textContent = 'Wrong password';
      errEl.style.display = '';
    }
  };

  document.getElementById('lock-reset-btn').onclick = async () => {
    await window.rterm.deleteVault();
    document.getElementById('lock-screen').style.display = 'none';
    window.__rterm_vault_pass = '';
    savedSessions.length = 0;
    setSavedSessionsDirty(true);
    renderSavedSessions();
  };

  document.getElementById('lock-pass').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') document.getElementById('lock-unlock-btn').click();
  });

  document.getElementById('lock-eye-toggle').onclick = () => {
    const input = document.getElementById('lock-pass');
    input.type = input.type === 'password' ? 'text' : 'password';
  };
}

import { commandHistory, localPath } from './state.js';
