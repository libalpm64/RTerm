// NOTE: This module does NOT import from ui.js. UI callers handle menu/modal closing.
import {
  sessions, activeId, fontSize, cursorStyle, cursorBlink, scrollback,
  currentTerm, currentFitAddon, commandHistory,
  savedSessions, savedSessionsDirty,
  _editingIndex, connecting,
  keysTabOpen, draggedTabId,
  setActiveId, setFontSize, setCursorStyle, setCursorBlink, setScrollback,
  setCurrentTerm, setCurrentFitAddon, setSavedSessionsDirty,
  setEditingIndex, setConnecting, setKeysTabOpen, setDraggedTabId
} from './state.js';
import { _ipc, invoke } from './ipc.js';
import { byId } from './dom.js';

export function showTerminalView() {
  document.getElementById('keys-panel').style.display = 'none';
  document.getElementById('keys-panel').classList.add('hidden');
  if (sessions.size > 0) {
    const hasTerminal = Array.from(sessions.values()).some(s => s.type !== 'keys');
    if (hasTerminal) {
      document.getElementById('terminal-container').classList.remove('hidden');
      document.getElementById('empty-state').style.display = 'none';
    }
  }
  if (!document.querySelector('#terminals > div[style*="display: block"]')) {
    const terminalSessions = Array.from(sessions.entries()).find(([id, s]) => s.type !== 'keys');
    if (terminalSessions) {
      showTerminal(terminalSessions[0]);
    }
  }
}

export function showKeysPanel() {
  document.getElementById('empty-state').style.display = 'none';
  document.getElementById('terminal-container').classList.add('hidden');
  const keysPanel = document.getElementById('keys-panel');
  keysPanel.classList.remove('hidden');
  keysPanel.style.display = 'flex';
  import('./keys.js').then(mod => mod.loadKeys());
}

export function renderTabs() {
  const tabbar = document.getElementById('tabbar');
  const tabAddBtn = tabbar.querySelector('.tab-add');

  const existingTabs = new Map();
  tabbar.querySelectorAll('.tab').forEach(t => existingTabs.set(t.dataset.id, t));

  existingTabs.forEach((tab, id) => {
    if (!sessions.has(id)) tab.remove();
  });

  sessions.forEach((sess, id) => {
    const dotColor = sess.type === 'local' ? 'var(--green)' : sess.type === 'ssh' ? 'var(--blue)' : sess.type === 'keys' ? 'var(--yellow)' : 'var(--purple)';
    const dotHtml = sess.type === 'keys' ?
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" width="12" height="12" class="tab-icon"><path d="M21 2l-2 2m-7.61 7.61a5.5 5.5 0 11-7.778 7.778 5.5 5.5 0 017.777-7.777zm0 0L15.5 7.5m0 0l3 3L22 7l-3-3m-3.5 3.5L19 4"/></svg>' :
      `<span class="tab-dot" style="background:${dotColor}" data-id="${id}"></span>`;

    let tab = existingTabs.get(id);
    if (!tab) {
      tab = document.createElement('div');
      tab.className = 'tab';
      tab.dataset.id = id;
      tab.innerHTML = `
        <span class="tab-indicator" data-id="${id}">${dotHtml}</span>
        <span class="tab-name">${sess.name}</span>
        <span class="tab-close" data-id="${id}">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" width="12" height="12"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>
        </span>
      `;
      tab.onclick = (e) => {
        if (e.target.closest('.tab-close')) {
          closeTab(e.target.closest('.tab-close').dataset.id);
        } else {
          selectTab(id);
        }
      };
      tab.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        showTabMenu(e, id);
      });
      tab.setAttribute('draggable', 'true');
      tab.addEventListener('dragstart', handleTabDragStart);
      tab.addEventListener('dragend', handleTabDragEnd);
      tab.addEventListener('dragover', handleTabDragOver);
      tab.addEventListener('drop', handleTabDrop);
      tab.addEventListener('dragleave', handleTabDragLeave);
      tabbar.insertBefore(tab, tabAddBtn);
    } else {
      tab.querySelector('.tab-indicator').innerHTML = dotHtml;
      tab.querySelector('.tab-name').textContent = sess.name;
    }

    tab.className = 'tab' + (id === activeId ? ' active' : '');
  });

  document.getElementById('current-session').textContent = sessions.get(activeId)?.name || 'No session';
}

export function closeTab(id) {
  const sess = sessions.get(id);

  if (sess?.type === 'keys') {
    setKeysTabOpen(false);
  } else {
    const termDiv = document.getElementById('term-' + id);
    if (termDiv) termDiv.remove();
    if (sess?.term) {
      sess.term.dispose();
    }
  }

  sessions.delete(id);

  if (activeId === id || !sessions.has(activeId)) {
    const keys = Array.from(sessions.keys());
    setActiveId(keys.length > 0 ? keys[keys.length - 1] : null);
  }

  renderTabs();

  if (sessions.size === 0) {
    document.getElementById('terminal-container').classList.add('hidden');
    document.getElementById('keys-panel').style.display = 'none';
    document.getElementById('empty-state').classList.remove('hidden');
    setCurrentTerm(null);
    setCurrentFitAddon(null);
  } else if (activeId) {
    selectTab(activeId);
  }
}

export function selectTab(id) {
  setActiveId(id);
  renderTabs();
  const sess = sessions.get(id);
  if (sess) {
    if (sess.type === 'keys') {
      showKeysPanel();
    } else {
      showTerminal(id);
      showTerminalView();
    }
  }
}

export function showTerminal(id) {
  const sess = sessions.get(id);
  if (!sess || !sess.term) return;

  document.querySelectorAll('div[id^="term-"]').forEach(d => {
    d.style.display = d.id === 'term-' + id ? 'block' : 'none';
  });

  setCurrentTerm(sess.term);
  setCurrentFitAddon(sess.fitAddon);

  fitAll();
  updateStatus(id);
}

export function updateStatus(id) {
  const sess = sessions.get(id) || sessions.get(activeId);
  if (sess && sess.term) {
    document.getElementById('status-size').textContent = sess.term.cols + 'x' + sess.term.rows;
    document.getElementById('status-cursor').textContent = 'Ln ' + (sess.term.buffer.active.cursorY + 1) + ', Col ' + (sess.term.buffer.active.cursorX + 1);
    document.getElementById('status-mode').textContent = sess.type === 'ssh' ? 'SSH' : 'Local';
  }
  const now = new Date();
  document.getElementById('status-time').textContent = now.toLocaleTimeString();
}

export function showTabMenu(e, id) {
  e.stopPropagation();
  let menu = document.getElementById('tab-context-menu');
  if (!menu) {
    menu = document.createElement('div');
    menu.id = 'tab-context-menu';
    menu.style.cssText = 'position:fixed;background:var(--bg3);border:1px solid var(--border2);border-radius:6px;padding:4px;z-index:3000;min-width:150px;box-shadow:0 8px 24px rgba(0,0,0,.5);display:none;';
    document.body.appendChild(menu);
  }

  const sess = sessions.get(id);
  const isKeysTab = sess?.type === 'keys';

  menu.innerHTML = `
    ${isKeysTab ? '' : '<div class="dropdown-item" data-action="rename">Rename</div>'}
    ${isKeysTab ? '' : '<div class="dropdown-item" data-action="duplicate">Duplicate</div>'}
    ${isKeysTab ? '' : '<div class="dropdown-divider"></div>'}
    <div class="dropdown-item" data-action="close">Close</div>
    <div class="dropdown-item" data-action="close-others">Close Others</div>
    <div class="dropdown-item" data-action="close-all">Close All</div>
  `;

  menu.querySelectorAll('.dropdown-item').forEach(item => {
    item.onclick = () => {
      const action = item.dataset.action;
      if (action === 'close') closeTab(id);
      else if (action === 'close-others') {
        Array.from(sessions.keys()).filter(sid => sid !== id).forEach(sid => closeTab(sid));
      }
      else if (action === 'close-all') {
        Array.from(sessions.keys()).forEach(sid => closeTab(sid));
      }
      else if (action === 'duplicate' && !isKeysTab) {
        const sess = sessions.get(id);
        if (sess) newTerminal(sess.type);
      }
      else if (action === 'rename' && !isKeysTab) {
        const tab = document.querySelector(`.tab[data-id="${id}"] .tab-name`);
        if (tab) {
          const newName = prompt('Enter new name:', sess.name);
          if (newName) {
            sess.name = newName;
            renderTabs();
          }
        }
      }
      menu.style.display = 'none';
    };
  });

  menu.style.left = e.clientX + 'px';
  menu.style.top = e.clientY + 'px';
  menu.style.display = 'block';

  const closeMenuFn = (ev) => {
    if (!menu.contains(ev.target)) {
      menu.style.display = 'none';
      document.removeEventListener('click', closeMenuFn);
    }
  };
  setTimeout(() => document.addEventListener('click', closeMenuFn), 10);
}


export function handleTabDragStart(e) {
  setDraggedTabId(e.currentTarget.dataset.id);
  e.currentTarget.classList.add('dragging');
  e.dataTransfer.effectAllowed = 'move';
}

export function handleTabDragEnd(e) {
  e.currentTarget.classList.remove('dragging');
  document.querySelectorAll('.tab').forEach(t => t.classList.remove('drag-over'));
  document.querySelectorAll('.drop-indicator').forEach(i => i.remove());
  setDraggedTabId(null);
}

export function handleTabDragOver(e) {
  e.preventDefault();
  if (!draggedTabId || e.currentTarget.dataset.id === draggedTabId) return;

  document.querySelectorAll('.tab').forEach(t => t.classList.remove('drag-over'));
  e.currentTarget.classList.add('drag-over');

  const rect = e.currentTarget.getBoundingClientRect();
  const isLeft = e.clientX < rect.left + rect.width / 2;

  document.querySelectorAll('.drop-indicator').forEach(i => i.remove());

  const indicator = document.createElement('div');
  indicator.className = 'drop-indicator ' + (isLeft ? 'vertical' : 'vertical');
  indicator.style.top = rect.top + 'px';
  indicator.style.height = rect.height + 'px';
  indicator.style.left = (isLeft ? rect.left : rect.right - 4) + 'px';
  document.body.appendChild(indicator);
}

export function handleTabDragLeave(e) {
  if (!e.currentTarget.contains(e.relatedTarget)) {
    e.currentTarget.classList.remove('drag-over');
    document.querySelectorAll('.drop-indicator').forEach(i => i.remove());
  }
}

export function handleTabDrop(e) {
  e.preventDefault();
  if (!draggedTabId) return;
  document.querySelectorAll('.drop-indicator').forEach(x => x.remove());
  document.querySelectorAll('.tab.drag-over').forEach(x => x.classList.remove('drag-over'));
}

export function toggleSidebar(side) {
  const el = side === 'left' ? document.getElementById('sidebar') : document.getElementById('right-sidebar');
  el.classList.toggle('collapsed');
}

export function zoomIn() {
  setFontSize(Math.min(48, fontSize + 2));
  sessions.forEach((sess) => {
    if (sess.term) {
      sess.term.options.fontSize = fontSize;
      sess.fitAddon?.fit();
    }
  });
  updateStatus();
}

export function zoomOut() {
  setFontSize(Math.max(8, fontSize - 2));
  sessions.forEach((sess) => {
    if (sess.term) {
      sess.term.options.fontSize = fontSize;
      sess.fitAddon?.fit();
    }
  });
  updateStatus();
}

export function resetZoom() {
  setFontSize(13);
  sessions.forEach((sess) => {
    if (sess.term) {
      sess.term.options.fontSize = fontSize;
      sess.fitAddon?.fit();
    }
  });
  updateStatus();
}

export function copySelection() {
  if (!currentTerm) return;
  const sel = currentTerm.getSelection();
  if (sel && sel.length > 0) {
    currentTerm.copySelection();
    return;
  }
  const full = currentTerm.buffer.active;
  const lines = [];
  for (let i = 0; i < full.length; i++) {
    lines.push(full.getLine(i)?.translateToString(true) || '');
  }
  navigator.clipboard.writeText(lines.join('\n'));
}

export function pasteClipboard() {
  navigator.clipboard.readText().then(t => {
    if (currentTerm) currentTerm.paste(t);
  });
}

export function selectAll() {
  if (currentTerm) currentTerm.selectAll();
}

export function clearTerminal() {
  if (currentTerm) {
    currentTerm.clear();
    currentTerm.write('\x1b[1;32mTerminal cleared\x1b[0m\r\n$ ');
  }
}

export function showContextMenu(e) {
  e.preventDefault();
  const menu = document.getElementById('context-menu');
  menu.style.left = e.clientX + 'px';
  menu.style.top = e.clientY + 'px';
  menu.classList.add('show');
}

export function showQuickMenu(e) {
  e.preventDefault();
  e.stopPropagation();
  const menu = document.getElementById('context-menu');
  menu.style.left = e.clientX + 'px';
  menu.style.top = e.clientY + 'px';
  menu.classList.add('show');
}

export function addHistoryItem(cmd) {
  if (commandHistory[0] === cmd) return;
  const idx = commandHistory.indexOf(cmd);
  if (idx !== -1) commandHistory.splice(idx, 1);
  commandHistory.unshift(cmd);
  if (commandHistory.length > 50) commandHistory.pop();
  renderHistory();
  window.rterm?.saveSetting('command_history', JSON.stringify(commandHistory));
}

export function renderHistory() {
  const list = byId('history-list');
  if (!list) return;
  list.replaceChildren();

  if (commandHistory.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'sempty';
    empty.textContent = 'No command history';
    list.appendChild(empty);
    return;
  }

  const fragment = document.createDocumentFragment();
  commandHistory.forEach(cmd => {
    const item = document.createElement('div');
    item.className = 'history-item';
    item.addEventListener('click', () => executeHistory(cmd));

    const dot = document.createElement('span');
    dot.className = 'dot';
    item.append(dot, document.createTextNode(cmd));
    fragment.appendChild(item);
  });
  list.appendChild(fragment);
}

export function executeHistory(cmd) {
  const sess = sessions.get(activeId);
  if (sess && sess.term && sess.type === 'ssh' && sess.sshId !== null) {
    window.rterm.sshWrite(sess.sshId, cmd + '\n');
  }
}

export async function newTerminal(type = 'local') {
  const id = 'session-' + Date.now();
  let name = 'Terminal ' + (sessions.size + 1);

  sessions.set(id, { name, type, term: null, fitAddon: null, buffer: '' });
  setActiveId(id);

  renderTabs();
  createTerminalInstance(id);
  updateSplitLayout();
}

export function updateSplitLayout() {
  fitAll();
}

export function createTerminalInstance(id, onReady) {
  const sess = sessions.get(id);
  if (!sess) return;

  const emptyState = document.getElementById('empty-state');
  const terminalContainer = document.getElementById('terminal-container');
  emptyState.classList.add('hidden');
  terminalContainer.classList.remove('hidden');

  const container = document.getElementById('terminals');
  let termDiv = document.getElementById('term-' + id);
  if (!termDiv) {
    termDiv = document.createElement('div');
    termDiv.id = 'term-' + id;
    termDiv.style.cssText = 'flex:1;display:block;min-width:0;min-height:0;width:100%;height:100%;overflow:hidden;';
    container.appendChild(termDiv);
  }

  const term = new Terminal({
    cursorBlink: cursorBlink,
    cursorStyle: cursorStyle,
    theme: { background: '#000', cursor: '#4fc3f7', foreground: '#e0e0e0' },
    fontSize: fontSize,
    fontFamily: 'JetBrains Mono, monospace',
    allowProposedApi: true,
    scrollback: scrollback,
  });

  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);

  sess.term = term;
  sess.fitAddon = fitAddon;

  term.open(termDiv);

  requestAnimationFrame(() => fitAddon.fit());

  if (onReady) {
    onReady(term, sess);
  }

  if (!sess.cmdBuffer) sess.cmdBuffer = '';

  term.onData(data => {
    const sess = sessions.get(id);
    if (sess?.type === 'ssh' || sess?.type === 'telnet' || sess?.type === 'serial') {
      if (sess.sshId !== null && sess.sshId !== undefined && window.rterm) {
        if (sess.type === 'ssh') {
          if (data === '\r') {
            if (sess.cmdBuffer && sess.cmdBuffer.trim()) {
              addHistoryItem(sess.cmdBuffer.trim());
            }
            sess.cmdBuffer = '';
            window.rterm.sshWrite(sess.sshId, '\n');
          } else if (data === '\x7f') {
            sess.cmdBuffer = sess.cmdBuffer.slice(0, -1);
            window.rterm.sshWrite(sess.sshId, '\x7f');
          } else if (data.length === 1 && data.charCodeAt(0) >= 32 && data.charCodeAt(0) <= 126) {
            sess.cmdBuffer += data;
            window.rterm.sshWrite(sess.sshId, data);
          } else {
            window.rterm.sshWrite(sess.sshId, data);
          }
        } else {
          let sendData = data;
          if (data === '\r' && sess.type === 'telnet') sendData = '\r\n';
          window.rterm.sshWrite(sess.sshId, sendData).catch(err => {
            console.error('Failed to write to ' + sess.type + ':', err);
          });
        }
      }
    } else if (invoke) {
      invoke('write_to_session', { id: id, data }).catch(console.error);
    } else if (sess?.type === 'local' && window.rterm) {
      if (!sess.buffer) sess.buffer = '';
      if (data === '\r' || data === '\n') {
        const cmd = sess.buffer;
        sess.buffer = '';
        term.write('\r\n');
        if (cmd.trim()) {
          window.rterm.localExec(cmd, term.cols).then(resp => {
            if (resp && resp.result) {
              term.write(resp.result);
            }
            if (!resp || resp.error) {
              term.write('\r\nCommand failed: ' + (resp?.error || 'unknown'));
            }
            term.write('$ ');
          });
        } else {
          term.write('$ ');
        }
      } else if (data === '\x7f' || data === '\b') {
        if (sess.buffer.length > 0) {
          sess.buffer = sess.buffer.slice(0, -1);
          term.write('\b \b');
        }
      } else if (data.length === 1) {
        sess.buffer += data;
        term.write(data);
      }
    }
  });

  term.onResize(({ cols, rows }) => {
    const sess = sessions.get(id);
    if ((sess?.type === 'ssh' || sess?.type === 'telnet' || sess?.type === 'serial') && sess?.sshId !== null && sess?.sshId !== undefined && window.rterm) {
      window.rterm.sshResize(sess.sshId, cols, rows);
    }
  });

  term.onCursorMove(() => updateStatus(id));

  if (!invoke && !window.rterm) {
    if (sess?.type === 'ssh' || sess?.type === 'telnet' || sess?.type === 'serial') {
      term.write('\x1b[1;32mConnected to SSH session\x1b[0m\r\n\x1b[1;34m$\x1b[0m ');
    } else {
      term.write('\x1b[1;32mWelcome to Rterm.\x1b[0m\r\n$ ');
    }
  } else if (sess?.type === 'local' || sess?.type === 'keys') {
    term.write('\x1b[1;32mLocal Terminal\x1b[0m\r\n$ ');
  }

  updateSplitLayout();
  showTerminal(id);
}

export function updateFontSize(size) {
  setFontSize(parseInt(size));
  sessions.forEach(sess => {
    if (sess.term) {
      sess.term.options.fontSize = fontSize;
      if (sess.fitAddon) sess.fitAddon.fit();
    }
  });
  window.rterm?.saveSetting('font_size', fontSize.toString());
}

export function updateScrollback(value) {
  let val = parseInt(value);
  if (val <= 0) val = 1000000;
  setScrollback(val);
  sessions.forEach(sess => {
    if (sess.term) {
      sess.term.options.scrollback = scrollback;
    }
  });
  window.rterm?.saveSetting('scrollback', value.toString());
}

export function updateCursorStyle(style) {
  setCursorStyle(style);
  sessions.forEach((sess) => {
    if (sess.term) sess.term.options.cursorStyle = style;
  });
}

export function updateCursorBlink(enabled) {
  setCursorBlink(enabled);
  sessions.forEach((sess) => {
    if (sess.term) sess.term.options.cursorBlink = enabled;
  });
  window.rterm?.saveSetting('cursor_blink', enabled ? '1' : '0');
}

export function updateTheme(name) {
  document.documentElement.className = name === 'default' ? '' : 'theme-' + name;
  window.rterm?.saveSetting('theme', name);
}

export function updateBell(enabled) {
  window.rterm?.saveSetting('bell_enabled', enabled ? '1' : '0');
}

export function updateCopySelect(enabled) {
  window.rterm?.saveSetting('copy_on_select', enabled ? '1' : '0');
}

export async function doConnect() {
  if (connecting) { console.log('doConnect: already connecting'); return; }
  setConnecting(true);

  const activeTab = document.querySelector('#modal-connect .modal-tab.active');
  const type = activeTab?.dataset.type || 'local';
  console.log('doConnect called, type:', type);

  let sshId = null;

  if (type === 'ssh') {
    const name = document.getElementById('ssh-name').value || (document.getElementById('ssh-user').value + '@' + document.getElementById('ssh-host').value);
    const host = document.getElementById('ssh-host').value;
    const save = document.getElementById('ssh-save-toggle').classList.contains('on');

    const authMethod = document.getElementById('ssh-auth-method')?.value || 'password';
    const keySel = document.getElementById('ssh-key-select');
    const config = {
      host: host,
      port: parseInt(document.getElementById('ssh-port').value) || 22,
      user: document.getElementById('ssh-user').value,
      password: authMethod === 'password' ? (document.getElementById('ssh-pass').value || null) : null,
      key_path: null,
      key_name: null,
      vault_pass: null,
      compression: document.getElementById('setting-compression')?.classList.contains('on') || false,
    };
    if (authMethod === 'key' && keySel?.value) {
      const val = keySel.value;
      const vaultKey = (window.__rterm_vault_keys || []).find(k => k.name === val);
      if (vaultKey && window.__rterm_vault_pass) {
        config.key_name = val;
        config.vault_pass = window.__rterm_vault_pass;
      } else {
        config.key_path = val;
      }
    }

    if (save) {
      const sessionData = {
        host: config.host, port: config.port,
        user: config.user, password: config.password,
        key_path: config.key_path, key_name: config.key_name,
        vault_pass: config.vault_pass, name
      };
      if (_editingIndex >= 0) {
        savedSessions[_editingIndex] = { ...savedSessions[_editingIndex], ...sessionData };
        setEditingIndex(-1);
        setSavedSessionsDirty(true);
        import('./sessions.js').then(mod => mod.renderSavedSessions());
      } else {
        import('./sessions.js').then(mod => mod.addSession('ssh', sessionData, true));
      }
      if (window.__rterm_vault_pass) {
        window.rterm.saveSessions(savedSessions.map(s => ({
          host: s.host, port: s.port,
          user: s.user, password: s.password,
          key_path: s.key_path, key_name: s.key_name,
          vault_pass: s.vault_pass, name: s.name
        })), window.__rterm_vault_pass);
      }
    }

    const id = 'session-' + Date.now();
    sessions.set(id, { name, type: 'ssh', term: null, fitAddon: null, sshId: null });
    setActiveId(id);
    renderTabs();
    createTerminalInstance(id);

    setTimeout(async () => {
      try {
        const result = await window.rterm.sshConnect(config);
        if (!result || !result.success) {
          console.error('Connection failed:', result?.error);
          closeTab(id);
          setConnecting(false);
          return;
        }
        sshId = result.id;
        const sess = sessions.get(id);
        if (sess) sess.sshId = sshId;

        const shellResult = await window.rterm.sshShell(sshId);
        if (!shellResult || !shellResult.success) {
          console.error('Shell failed:', shellResult?.error);
          closeTab(id);
          setConnecting(false);
          return;
        }

        const sess2 = sessions.get(id);
        if (sess2 && sess2.term) {
          setTimeout(() => {
            sess2.term.focus();
            const ta = document.querySelector('.xterm-helper-textarea');
            if (ta) ta.focus();
            if (sess2.fitAddon) {
              sess2.fitAddon.fit();
              const dims = sess2.fitAddon.proposeDimensions();
              if (dims) window.rterm?.sshResize(sess2.sshId, dims.cols, dims.rows);
            }
          }, 100);
        }
        window.dispatchEvent(new CustomEvent('rterm:ssh-connected', {
          detail: { sessionId: id, sshId }
        }));
      } catch (e) {
        console.error('Connection error:', e);
        closeTab(id);
      }
      setConnecting(false);
    }, 100);
    return;
  }

  if (type === 'telnet') {
    const host = document.getElementById('telnet-host').value;
    const port = parseInt(document.getElementById('telnet-port').value) || 23;
    const id = 'session-' + Date.now();
    sessions.set(id, { name: host, type: 'telnet', term: null, fitAddon: null, sshId: null });
    setActiveId(id);
    renderTabs();
    createTerminalInstance(id);

    setTimeout(async () => {
      const res = await window.rterm.telnetConnect({ host, port });
      if (res.success) {
        const s = sessions.get(id);
        if (s) { s.sshId = res.id; s.term.write('\r\nConnected to ' + host + '\r\n'); setTimeout(() => s.term.focus(), 100); }
      } else {
        const s = sessions.get(id);
        if (s && s.term) s.term.write('\r\nConnection failed: ' + res.error + '\r\n');
        setTimeout(() => closeTab(id), 3000);
      }
      setConnecting(false);
    }, 100);
    return;
  }

  if (type === 'serial') {
    const port = document.getElementById('serial-port').value;
    const baud = parseInt(document.getElementById('serial-baud').value) || 9600;
    const id = 'session-' + Date.now();
    sessions.set(id, { name: port, type: 'serial', term: null, fitAddon: null, sshId: null });
    setActiveId(id);
    renderTabs();
    createTerminalInstance(id);

    setTimeout(async () => {
      const res = await window.rterm.serialConnect({ port, baud });
      if (res.success) {
        const s = sessions.get(id);
        if (s) { s.sshId = res.id; s.term.write('\r\nOpened ' + port + ' @ ' + baud + '\r\n'); setTimeout(() => s.term.focus(), 100); }
      } else {
        const s = sessions.get(id);
        if (s && s.term) s.term.write('\r\nFailed to open port: ' + res.error + '\r\n');
        setTimeout(() => closeTab(id), 3000);
      }
      setConnecting(false);
    }, 100);
    return;
  }

  const id = 'session-' + Date.now();
  sessions.set(id, { name: type, type, term: null, fitAddon: null, sshId: null });
  setActiveId(id);
  renderTabs();
  createTerminalInstance(id);
  setConnecting(false);
}

let _fitRAFInternal = null;
export function fitAll() {
  if (_fitRAFInternal) return;
  _fitRAFInternal = requestAnimationFrame(() => {
    _fitRAFInternal = null;
    sessions.forEach((sess, id) => {
      if (!sess.fitAddon) return;
      try { sess.fitAddon.fit(); } catch (_) { }
      if (sess.type === 'ssh' && sess.sshId != null && sess.term) {
        const dims = sess.fitAddon.proposeDimensions();
        if (dims) window.rterm?.sshResize(sess.sshId, dims.cols, dims.rows);
      }
    });
  });
}
