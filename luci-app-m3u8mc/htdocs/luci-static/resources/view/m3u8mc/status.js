'use strict';
'require view';
'require fs';
'require poll';
'require uci';
'require ui';
'require dom';

var CTL = '/usr/bin/m3u8mcctl';

function esc(s) { return (s == null || s === '') ? '—' : String(s); }

function fmtUptime(s) {
	var h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
	return (h ? h + _(' 小时 ') : '') + m + _(' 分钟');
}

function fmtBytes(b) {
	if (!b) return '0';
	if (b < 1048576) return (b / 1024).toFixed(0) + ' KB';
	return (b / 1048576).toFixed(1) + ' MB';
}

function badge(text, kind) {
	return E('span', { 'class': 'm3u8mc-badge m3u8mc-' + kind }, text);
}

return view.extend({
	selected: {},
	sock: '/var/run/m3u8mcd.sock',

	load: function() {
		return uci.load('m3u8mc').then(function() {
			return uci.get('m3u8mc', 'global', 'control_socket') || '/var/run/m3u8mcd.sock';
		});
	},

	ctl: function(args) {
		return fs.exec_direct(CTL, [ '--socket', this.sock ].concat(args));
	},

	fetchStatus: function() {
		return this.ctl([ 'status', '--json' ]).then(function(out) {
			return JSON.parse(out);
		});
	},

	runAction: function(action, ids) {
		var self = this;
		if (!ids.length) {
			ui.addNotification(null, E('p', _('请先勾选频道。')), 'info');
			return Promise.resolve();
		}
		return Promise.all(ids.map(function(id) {
			return self.ctl([ 'channel', action, '--name', id ]).then(function() {
				return null;
			}).catch(function(e) {
				return id + '：' + (e.message || e);
			});
		})).then(function(errs) {
			errs = errs.filter(Boolean);
			if (errs.length)
				ui.addNotification(null, E('p', _('部分操作失败：') + errs.join('；')), 'danger');
			return self.refresh();
		});
	},

	renderTable: function(st) {
		var self = this;
		var chs = st.channels || [];

		/* 清理已不存在的勾选 */
		var alive = {};
		chs.forEach(function(c) { alive[c.id] = true; });
		Object.keys(this.selected).forEach(function(k) { if (!alive[k]) delete self.selected[k]; });

		var allBox = E('input', { 'type': 'checkbox', 'change': function(ev) {
			chs.forEach(function(c) { if (ev.target.checked) self.selected[c.id] = true; else delete self.selected[c.id]; });
			self.renderInto(st);
		} });
		allBox.checked = chs.length > 0 && chs.every(function(c) { return self.selected[c.id]; });

		var head = E('tr', { 'class': 'tr cbi-section-table-titles' }, [
			E('th', { 'class': 'th', 'style': 'width:2em' }, allBox),
			E('th', { 'class': 'th' }, _('频道')),
			E('th', { 'class': 'th' }, _('状态')),
			E('th', { 'class': 'th' }, _('模式')),
			E('th', { 'class': 'th' }, _('缓存')),
			E('th', { 'class': 'th' }, _('当前源')),
			E('th', { 'class': 'th' }, _('ffmpeg 重启')),
			E('th', { 'class': 'th' }, _('播放地址'))
		]);

		var rows = chs.map(function(c) {
			var box = E('input', { 'type': 'checkbox', 'change': function(ev) {
				if (ev.target.checked) self.selected[c.id] = true; else delete self.selected[c.id];
				self.updateToolbar();
			} });
			box.checked = !!self.selected[c.id];

			var stateBadge;
			if (!c.enabled) stateBadge = badge(_('已禁用'), 'gray');
			else if (!c.running) stateBadge = badge(_('已停止'), 'red');
			else if (c.overloaded) stateBadge = badge(_('过载'), 'yellow');
			else stateBadge = badge(_('运行中'), 'green');

			var modeText = { standard: _('缓存下载'), passthrough: _('直通'), proxy: _('HTTP 代理') }[c.mode] || c.mode;
			var cacheText = c.cache_storage
				? (c.cached_segments + _(' 片 · ') + (c.cache_storage === 'disk' ? _('本地目录') : _('内存')) + ' · ' + fmtBytes(c.cache_bytes))
				: '—';

			var tr = E('tr', { 'class': 'tr' }, [
				E('td', { 'class': 'td' }, box),
				E('td', { 'class': 'td' }, [ E('strong', {}, c.name || c.id), E('div', { 'class': 'm3u8mc-sub' }, c.id) ]),
				E('td', { 'class': 'td' }, stateBadge),
				E('td', { 'class': 'td' }, modeText),
				E('td', { 'class': 'td' }, cacheText),
				E('td', { 'class': 'td m3u8mc-sub' }, esc(c.active_server)),
				E('td', { 'class': 'td' }, String(c.ffmpeg_restarts || 0)),
				E('td', { 'class': 'td m3u8mc-sub' }, esc(c.play_url))
			]);
			var out = [ tr ];
			if (c.last_error) {
				out.push(E('tr', { 'class': 'tr' }, [
					E('td', { 'class': 'td' }),
					E('td', { 'class': 'td m3u8mc-err', 'colspan': 7 }, _('最近错误：') + c.last_error)
				]));
			}
			return out;
		});

		return E('table', { 'class': 'table cbi-section-table' },
			[ head ].concat([].concat.apply([], rows)));
	},

	updateToolbar: function() {
		var n = Object.keys(this.selected).length;
		var el = document.getElementById('m3u8mc-selcount');
		if (el) el.textContent = n ? (_('已选 ') + n + _(' 个')) : _('未选择');
	},

	renderInto: function(st) {
		var box = document.getElementById('m3u8mc-table');
		if (!box) return;
		dom.content(box, this.renderTable(st));
		this.updateToolbar();
		var up = document.getElementById('m3u8mc-uptime');
		if (up) up.textContent = _('守护进程已运行 ') + fmtUptime(st.uptime_secs || 0);
	},

	refresh: function() {
		var self = this;
		return this.fetchStatus().then(function(st) {
			self.lastStatus = st;
			self.renderInto(st);
		}).catch(function() {
			var box = document.getElementById('m3u8mc-table');
			if (box) dom.content(box, E('div', { 'class': 'alert-message warning' },
				_('无法连接守护进程。请确认服务已启用并在运行（系统 → 启动项 → m3u8mc）。')));
			var up = document.getElementById('m3u8mc-uptime');
			if (up) up.textContent = '';
		});
	},

	render: function(sock) {
		var self = this;
		this.sock = sock;

		function selIds() { return Object.keys(self.selected); }
		function btn(label, cls, fn) {
			return E('button', { 'class': 'btn cbi-button ' + cls, 'click': ui.createHandlerFn(self, fn) }, label);
		}

		var toolbar = E('div', { 'class': 'm3u8mc-toolbar' }, [
			btn(_('启动所选'), 'cbi-button-positive', function() { return self.runAction('start', selIds()); }),
			btn(_('停止所选'), 'cbi-button-negative', function() { return self.runAction('stop', selIds()); }),
			btn(_('重启所选'), 'cbi-button-action', function() { return self.runAction('restart', selIds()); }),
			E('span', { 'id': 'm3u8mc-selcount', 'class': 'm3u8mc-sub', 'style': 'margin-left:1em' }, _('未选择')),
			E('span', { 'id': 'm3u8mc-uptime', 'class': 'm3u8mc-sub', 'style': 'float:right' })
		]);

		var style = E('style', {}, [
			'.m3u8mc-toolbar{margin:.5em 0 1em}',
			'.m3u8mc-toolbar .btn{margin-right:.4em}',
			'.m3u8mc-sub{opacity:.7;font-size:.85em;word-break:break-all}',
			'.m3u8mc-err{color:#c0392b;font-size:.9em}',
			'.m3u8mc-badge{display:inline-block;padding:1px 8px;border-radius:10px;font-size:.85em;font-weight:600;color:#fff}',
			'.m3u8mc-green{background:#2e9e5b}.m3u8mc-red{background:#c0392b}',
			'.m3u8mc-yellow{background:#d68910}.m3u8mc-gray{background:#7f8c8d}'
		].join('\n'));

		var root = E('div', {}, [
			style,
			E('h2', {}, _('m3u8mc 运行状态')),
			toolbar,
			E('div', { 'id': 'm3u8mc-table' }, E('em', {}, _('正在加载…')))
		]);

		poll.add(L.bind(this.refresh, this), 4);
		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
