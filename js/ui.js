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
import { loadSftpDir, setupSaveDialog } from './sftp.js';
import { setupKeys } from './keys.js';
import { loadLocalDir } from './filer.js';

export function closeMenusInternal() {
  document.querySelectorAll('.dropdown').forEach(m => m.classList.remove('show'));
  document.querySelectorAll('.menu-btn').forEach(b => b.classList.remove('active'));
  setActiveMenu(null);
}

export function hideModalInternal() {
  document.getElementById('modal-overlay').classList.remove('show');
}

export function showModalInternal(type) {
  closeMenusInternal();
  document.getElementById('modal-connect').style.display = type === 'connect' ? 'block' : 'none';
  document.getElementById('modal-settings').style.display = type === 'settings' ? 'block' : 'none';
  document.getElementById('modal-about').style.display = type === 'about' ? 'block' : 'none';
  document.getElementById('modal-keys').style.display = type === 'keys' ? 'block' : 'none';
  document.getElementById('modal-overlay').classList.add('show');

  if (type === 'connect') {
    document.getElementById('modal-title').textContent = 'New Session';
    resetConnectModal();
  } else if (type === 'settings') {
    document.getElementById('modal-title').textContent = 'Settings';
    initSettingsTabs();
  }
}

function resetConnectModal() {
  setEditingIndex(-1);
  document.getElementById('modal-title').textContent = 'New Session';
  document.getElementById('modal-connect-btn').textContent = 'Connect';
  document.querySelectorAll('#modal-connect .modal-tab').forEach(t => t.classList.remove('active'));
  document.querySelector('#modal-connect .modal-tab[data-type="local"]').classList.add('active');
  document.getElementById('local-fields').style.display = '';
  document.getElementById('ssh-fields').style.display = 'none';
  document.getElementById('telnet-fields').style.display = 'none';
  document.getElementById('serial-fields').style.display = 'none';

  document.querySelectorAll('#modal-connect .toggle').forEach(t => t.classList.add('on'));
}

export function initSettingsTabs() {
  document.querySelectorAll('#modal-settings .modal-tab').forEach(t => t.classList.remove('active'));
  document.querySelector('#modal-settings .modal-tab[data-settings="terminal"]').classList.add('active');
  document.querySelectorAll('.settings-content').forEach(s => s.classList.remove('active'));
  document.getElementById('settings-terminal').classList.add('active');
}

export function setupEventListeners() {

  document.querySelectorAll('.menu-btn').forEach(btn => {
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      const menu = btn.querySelector('.dropdown');
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

  document.addEventListener('click', (e) => {
    if (!e.target.closest('.menu-btn') && !e.target.closest('.dropdown')) {
      closeMenusInternal();
    }
  });

  document.getElementById('modal-close-btn').addEventListener('click', hideModalInternal);
  document.getElementById('modal-overlay').addEventListener('click', (e) => {
    if (e.target === document.getElementById('modal-overlay')) hideModalInternal();
  });

  document.querySelectorAll('#modal-connect .modal-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      document.querySelectorAll('#modal-connect .modal-tab').forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      const type = tab.dataset.type;
      document.getElementById('local-fields').style.display = type === 'local' ? 'block' : 'none';
      document.getElementById('ssh-fields').style.display = type === 'ssh' ? 'block' : 'none';
      document.getElementById('telnet-fields').style.display = type === 'telnet' ? 'block' : 'none';
      document.getElementById('serial-fields').style.display = type === 'serial' ? 'block' : 'none';
    });
  });

  // SSH Auth method toggle
  document.getElementById('ssh-auth-method').addEventListener('change', async (e) => {
    const method = e.target.value;
    document.getElementById('ssh-password-fields').style.display = method === 'password' ? '' : 'none';
    const keyFields = document.getElementById('ssh-key-fields');
    keyFields.style.display = method === 'key' ? '' : 'none';
    if (method === 'key') {
      const sel = document.getElementById('ssh-key-select');
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

  document.querySelectorAll('#modal-settings .modal-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      document.querySelectorAll('#modal-settings .modal-tab').forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      const section = tab.dataset.settings;
      document.querySelectorAll('.settings-content').forEach(s => s.classList.remove('active'));
      document.getElementById('settings-' + section).classList.add('active');
    });
  });

  // Toggle switches
  document.querySelectorAll('.toggle').forEach(toggle => {
    toggle.addEventListener('click', () => toggle.classList.toggle('on'));
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

      if (panel === 'sftp') setTimeout(() => loadSftpDir('/'), 50);
      if (panel === 'local') setTimeout(() => loadLocalDir('~'), 50);
    });
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
    menu.innerHTML = '<div class="dropdown-item" id="sctx-ul">Upload File</div>';
    document.body.appendChild(menu);
    menu.style.left = Math.min(e.clientX, window.innerWidth - 180) + 'px';
    menu.style.top = Math.min(e.clientY, window.innerHeight - 80) + 'px';
    document.getElementById('sctx-ul').onclick = () => {
      menu.remove();
      alert('SFTP Upload: File picker integration pending');
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
    alert('SFTP Upload: File picker integration pending');
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

  document.getElementById('action-keys-agent').onclick = () => {
    const agentToggle = document.getElementById('setting-ssh-agent');
    if (agentToggle) agentToggle.classList.toggle('on');
  };

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
