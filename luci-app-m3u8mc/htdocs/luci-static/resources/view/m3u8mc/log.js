'use strict';
'require view';
'require fs';
'require poll';
'require dom';

/*
 * 运行日志：解析 m3u8mcd 的单行日志（"LEVEL [频道] 模块: 内容"），
 * 按级别着色，可按级别 / 频道 / 关键字过滤，自动刷新并滚动到最新。
 */

var LEVELS = { ERROR: 4, WARN: 3, INFO: 2, DEBUG: 1, TRACE: 0 };
var SYSLOG_RE = /^(\w{3}\s+\w{3}\s+\d+\s+(\d\d:\d\d:\d\d)\s+\d{4})\s+(\S+)\s+([^\s:\[]+)(?:\[\d+\])?:\s?(.*)$/;
var APP_RE = /^(ERROR|WARN|INFO|DEBUG|TRACE)\s+(?:\[([^\]]+)\]\s+)?([\w.\-]+):\s?(.*)$/;
// eslint-disable-next-line no-control-regex
var ANSI_RE = /\x1b\[[0-9;]*m/g;

function parseLine(raw) {
	raw = raw.replace(ANSI_RE, '');
	var o = { raw: raw, time: '', level: 'INFO', channel: '', module: '', msg: raw };
	var m = raw.match(SYSLOG_RE);
	var body = raw;
	if (m) { o.time = m[2]; body = m[5]; }
	var a = body.match(APP_RE);
	if (a) {
		o.level = a[1]; o.channel = a[2] || ''; o.module = a[3]; o.msg = a[4];
	} else {
		o.msg = body;
		if (/error|fail|panick/i.test(body)) o.level = 'ERROR';
	}
	return o;
}

return view.extend({
	auto: true,
	stick: true,
	entries: [],

	load: function() { return this.fetchLog(); },

	fetchLog: function() {
		return fs.exec_direct('/sbin/logread', [ '-e', 'm3u8mc' ]).then(function(out) {
			return (out || '').split('\n').filter(function(l) { return l.trim(); });
		}).catch(function() { return null; });
	},

	apply: function() {
		var lvl = document.getElementById('lg-level').value;
		var ch = document.getElementById('lg-chan').value;
		var kw = document.getElementById('lg-kw').value.trim().toLowerCase();
		var min = LEVELS[lvl] || 0;

		var chans = {};
		this.entries.forEach(function(e) { if (e.channel) chans[e.channel] = true; });
		var sel = document.getElementById('lg-chan');
		var cur = sel.value;
		var names = Object.keys(chans).sort();
		if (sel.options.length !== names.length + 1) {
			dom.content(sel, [ E('option', { 'value': '' }, _('全部频道')) ].concat(
				names.map(function(n) { return E('option', { 'value': n }, n); })));
			sel.value = cur;
		}

		var shown = this.entries.filter(function(e) {
			if ((LEVELS[e.level] || 0) < min) return false;
			if (ch && e.channel !== ch) return false;
			if (kw && e.raw.toLowerCase().indexOf(kw) < 0) return false;
			return true;
		}).slice(-600);

		var box = document.getElementById('lg-box');
		var rows = shown.map(function(e) {
			return E('div', { 'class': 'lg-row lg-' + e.level.toLowerCase() }, [
				E('span', { 'class': 'lg-time' }, e.time),
				E('span', { 'class': 'lg-lvl' }, e.level),
				E('span', { 'class': 'lg-ch' }, e.channel ? '[' + e.channel + ']' : ''),
				E('span', { 'class': 'lg-mod' }, e.module),
				E('span', { 'class': 'lg-msg' }, e.msg)
			]);
		});
		dom.content(box, rows.length ? rows : E('em', {}, _('没有符合条件的日志。')));
		document.getElementById('lg-count').textContent = shown.length + ' / ' + this.entries.length + _(' 行');
		if (this.stick) box.scrollTop = box.scrollHeight;
	},

	setLines: function(lines) {
		if (lines === null) {
			dom.content(document.getElementById('lg-box'), E('em', {}, _('读取系统日志失败（logread 不可用或无权限）。')));
			return;
		}
		this.entries = lines.map(parseLine);
		this.apply();
	},

	download: function() {
		var blob = new Blob([ this.entries.map(function(e) { return e.raw; }).join('\n') ], { type: 'text/plain' });
		var a = E('a', { 'href': URL.createObjectURL(blob), 'download': 'm3u8mc.log' });
		document.body.appendChild(a); a.click(); document.body.removeChild(a);
	},

	render: function(lines) {
		var self = this;
		var onchg = function() { self.apply(); };

		var style = E('style', {}, [
			'#lg-box{height:60vh;overflow:auto;font-family:monospace;font-size:.85em;border:1px solid rgba(127,127,127,.4);border-radius:4px;padding:4px 0}',
			'.lg-row{display:flex;gap:.6em;padding:1px 8px;border-left:3px solid transparent;white-space:pre-wrap;word-break:break-all}',
			'.lg-row:hover{background:rgba(127,127,127,.12)}',
			'.lg-time{opacity:.6;min-width:4.6em}.lg-lvl{min-width:3.6em;font-weight:700}',
			'.lg-ch{color:#2980b9;font-weight:600}.lg-mod{opacity:.6}.lg-msg{flex:1}',
			'.lg-error{border-left-color:#c0392b;background:rgba(192,57,43,.12)}.lg-error .lg-lvl{color:#c0392b}',
			'.lg-warn{border-left-color:#d68910;background:rgba(214,137,16,.10)}.lg-warn .lg-lvl{color:#d68910}',
			'.lg-info .lg-lvl{color:#2e9e5b}',
			'.lg-debug,.lg-trace{opacity:.65}',
			'.lg-bar{display:flex;flex-wrap:wrap;gap:.6em;align-items:center;margin:.5em 0}'
		].join('\n'));

		var level = E('select', { 'id': 'lg-level', 'class': 'cbi-input-select', 'change': onchg }, [
			E('option', { 'value': 'TRACE' }, _('全部级别')),
			E('option', { 'value': 'INFO', 'selected': 'selected' }, _('信息及以上')),
			E('option', { 'value': 'WARN' }, _('警告及以上')),
			E('option', { 'value': 'ERROR' }, _('仅错误'))
		]);
		var chan = E('select', { 'id': 'lg-chan', 'class': 'cbi-input-select', 'change': onchg },
			E('option', { 'value': '' }, _('全部频道')));
		var kw = E('input', { 'id': 'lg-kw', 'type': 'search', 'class': 'cbi-input-text', 'placeholder': _('搜索关键字'), 'input': onchg });

		var autoBox = E('input', { 'type': 'checkbox', 'checked': 'checked', 'change': function(ev) { self.auto = ev.target.checked; } });
		var stickBox = E('input', { 'type': 'checkbox', 'checked': 'checked', 'change': function(ev) { self.stick = ev.target.checked; if (self.stick) self.apply(); } });

		var root = E('div', {}, [
			style,
			E('h2', {}, _('m3u8mc 运行日志')),
			E('div', { 'class': 'lg-bar' }, [
				level, chan, kw,
				E('label', {}, [ autoBox, ' ', _('自动刷新') ]),
				E('label', {}, [ stickBox, ' ', _('滚动到最新') ]),
				E('button', { 'class': 'btn cbi-button', 'click': function() { self.fetchLog().then(L.bind(self.setLines, self)); } }, _('立即刷新')),
				E('button', { 'class': 'btn cbi-button', 'click': L.bind(this.download, this) }, _('下载')),
				E('span', { 'id': 'lg-count', 'style': 'opacity:.7' })
			]),
			E('div', { 'id': 'lg-box' })
		]);

		window.setTimeout(function() { self.setLines(lines); }, 0);
		poll.add(function() {
			if (!self.auto) return Promise.resolve();
			return self.fetchLog().then(L.bind(self.setLines, self));
		}, 3);
		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
