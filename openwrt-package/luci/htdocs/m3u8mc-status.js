/**
 * m3u8mc-status.js
 * 状态页面前端逻辑：轮询 daemon 状态、渲染频道表格、提供操作按钮。
 * 纯 ES5 兼容写法，无依赖，适配 OpenWrt 上的旧版内嵌浏览器。
 */

var M3u8mcStatus = (function () {
  var cfg = {};
  var timer = null;

  // ── 初始化 ─────────────────────────────────────────────────
  function init(options) {
    cfg = options;
    refresh();
    timer = setInterval(refresh, cfg.interval || 5000);
  }

  // ── 数据获取 ────────────────────────────────────────────────
  function refresh() {
    xhr(cfg.statusUrl, null, function (err, data) {
      if (err) {
        showErr('获取状态失败：' + err);
        return;
      }
      showErr(null);
      try {
        render(typeof data === 'string' ? JSON.parse(data) : data);
      } catch (e) {
        showErr('JSON 解析失败：' + e.message);
      }
    });
    var ts = new Date().toLocaleTimeString();
    var el = doc('mc-refresh-ts');
    if (el) el.textContent = '最后刷新：' + ts;
  }

  // ── 渲染 ────────────────────────────────────────────────────
  function render(status) {
    var uptimeEl = doc(cfg.uptimeId);
    if (uptimeEl) uptimeEl.textContent = '守护进程已运行 ' + fmtUptime(status.uptime_secs);

    var tableEl = doc(cfg.tableId);
    if (!tableEl) return;

    var rows = (status.channels || []).map(function (ch) {
      return '<tr>' +
        td(ch.id) +
        td(ch.name) +
        pill(ch.enabled, ch.enabled ? '已启用' : '已禁用') +
        pill(ch.running, ch.running ? '运行中' : '已停止') +
        td(ch.mode === 'standard' ? '标准' : '直通') +
        td(ch.cached_segments) +
        td(ch.ffmpeg_pid ? ch.ffmpeg_pid : '—') +
        td(ch.ffmpeg_restarts) +
        pill(ch.overloaded, ch.overloaded ? '过载' : '正常', ch.overloaded ? 'yellow' : 'green') +
        td('<a href="' + escAttr(ch.play_url) + '" style="font-size:0.8em">' + esc(ch.play_url) + '</a>') +
        td(actionButtons(ch)) +
        '</tr>';
    }).join('');

    tableEl.innerHTML =
      '<table class="mc-table">' +
      '<thead><tr>' +
      th('ID') + th('频道名') + th('启用') + th('状态') +
      th('模式') + th('缓存分片') + th('PID') + th('重启次数') +
      th('背压') + th('组播地址') + th('操作') +
      '</tr></thead>' +
      '<tbody>' + (rows || '<tr><td colspan="11">无频道运行</td></tr>') + '</tbody>' +
      '</table>';

    // 绑定操作按钮事件（事件委托）
    tableEl.onclick = function (e) {
      var btn = e.target;
      if (!btn || btn.tagName !== 'BUTTON') return;
      var action = btn.getAttribute('data-action');
      var name   = btn.getAttribute('data-name');
      if (!action || !name) return;
      channelAction(action, name, btn);
    };
  }

  function actionButtons(ch) {
    var btns = '';
    if (ch.running) {
      btns += btn('stop',    ch.id, '停止');
      btns += btn('restart', ch.id, '重启');
    } else {
      btns += btn('start',   ch.id, '启动');
    }
    return btns;
  }

  function btn(action, name, label) {
    return '<button class="mc-act-btn" data-action="' +
      escAttr(action) + '" data-name="' + escAttr(name) + '">' + label + '</button>';
  }

  // ── 频道操作 ────────────────────────────────────────────────
  function channelAction(action, name, btnEl) {
    if (btnEl) btnEl.disabled = true;
    var fd = 'action=' + encodeURIComponent(action) +
             '&name='  + encodeURIComponent(name);
    xhr(cfg.actionUrl, fd, function (err) {
      if (err) { alert('操作失败：' + err); }
      if (btnEl) btnEl.disabled = false;
      setTimeout(refresh, 500); // 操作后立即刷新一次
    });
  }

  // ── 工具函数 ────────────────────────────────────────────────
  function xhr(url, body, cb) {
    var req = new XMLHttpRequest();
    req.open(body ? 'POST' : 'GET', url, true);
    if (body) req.setRequestHeader('Content-Type', 'application/x-www-form-urlencoded');
    req.onreadystatechange = function () {
      if (req.readyState !== 4) return;
      if (req.status === 200) {
        cb(null, req.responseText);
      } else {
        cb('HTTP ' + req.status);
      }
    };
    req.send(body || null);
  }

  function fmtUptime(secs) {
    var h = Math.floor(secs / 3600);
    var m = Math.floor((secs % 3600) / 60);
    var s = secs % 60;
    return (h ? h + ' 小时 ' : '') + (m ? m + ' 分 ' : '') + s + ' 秒';
  }

  function doc(id) { return document.getElementById(id); }

  function showErr(msg) {
    var el = doc(cfg.errId);
    if (!el) return;
    if (msg) { el.textContent = msg; el.style.display = ''; }
    else      { el.textContent = '';  el.style.display = 'none'; }
  }

  function esc(s) {
    return String(s)
      .replace(/&/g, '&amp;').replace(/</g, '&lt;')
      .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
  }
  function escAttr(s) { return esc(s); }
  function td(content) { return '<td>' + (content == null ? '—' : content) + '</td>'; }
  function th(label)   { return '<th>' + esc(label) + '</th>'; }
  function pill(cond, label, style) {
    var cls = style ||
      (cond  === true  ? 'green' :
       cond  === false ? 'red'   : 'gray');
    return '<td><span class="mc-pill mc-pill-' + cls + '">' + esc(label) + '</span></td>';
  }

  return { init: init, refresh: refresh };
}());
