'use strict';
'require view';
'require form';
'require uci';
'require ui';

/*
 * m3u8mc 频道管理
 *  - 顶部：全局设置
 *  - 下面：频道表格（行内勾选启用/停用，点“编辑”进入分页签的详细设置）
 *  - “快速添加”：一次粘贴多行「名称,m3u8 地址」，自动分配组播地址/端口，勾选常用选项即可创建多个频道
 */

function nextFreeId(existing) {
	var n = 1;
	while (existing[('ch' + n)]) n++;
	return 'ch' + n;
}

return view.extend({
	load: function() {
		return uci.load([ 'm3u8mc', 'network' ]);
	},

	render: function() {
		var m, s, o;

		m = new form.Map('m3u8mc', _('m3u8mc 频道管理'),
			_('把 HTTP 单播 m3u8 直播源转成 UDP/RTP 组播。修改后点击页面底部的“保存并应用”，只会热重载有变化的频道。'));

		/* ───────── 全局设置 ───────── */
		s = m.section(form.NamedSection, 'global', 'global', _('全局设置'));
		s.addremove = false;

		o = s.option(form.Flag, 'enabled', _('启用服务'));
		o.default = o.enabled = '1';
		o.rmempty = false;

		o = s.option(form.ListValue, 'log_level', _('日志级别'),
			_('排查问题时选“调试”，平时用“警告”即可。'));
		o.value('error', _('仅错误'));
		o.value('warning', _('警告（默认）'));
		o.value('info', _('信息'));
		o.value('m3u8mcd=debug,warn', _('调试'));
		o.default = 'warning';

		o = s.option(form.Value, 'http_port_base', _('内部 HTTP 起始端口'),
			_('未手动指定内部端口的频道，从这个端口开始依次自动分配。'));
		o.datatype = 'port';
		o.placeholder = '18080';

		o = s.option(form.Value, 'cache_dir', _('磁盘缓存根目录'),
			_('仅对“缓存位置=本地目录”的频道生效，实际使用 <根目录>/<频道ID>/。OpenWrt（含 x86）的 /tmp 通常是内存盘（tmpfs），放在那里并不省内存，想省内存请指向 USB/SD/硬盘挂载点；若目录实际落在内存盘上，启动时日志会给出警告。'));
		o.placeholder = '/tmp/m3u8mc';

		o = s.option(form.Value, 'control_socket', _('控制套接字'));
		o.placeholder = '/var/run/m3u8mcd.sock';
		o.advanced = true;

		/* ───────── 频道表格 ───────── */
		s = m.section(form.GridSection, 'channel', _('频道'));
		s.addremove = true;
		s.anonymous = false;
		s.nodescriptions = true;
		s.modaltitle = function(sid) { return _('编辑频道') + '：' + sid; };
		s.addbtntitle = _('添加单个频道');

		s.tab('basic', _('基本'));
		s.tab('download', _('源与下载'));
		s.tab('cache', _('缓存'));
		s.tab('ffmpeg', _('ffmpeg'));
		s.tab('advanced', _('高级'));

		/* —— 表格列 —— */
		o = s.option(form.Flag, 'enabled', _('启用'));
		o.editable = true;
		o.default = o.enabled = '1';
		o.rmempty = false;
		o.modalonly = false;

		o = s.option(form.DummyValue, '_name', _('名称'));
		o.modalonly = false;
		o.textvalue = function(sid) {
			return uci.get('m3u8mc', sid, 'name') || sid;
		};

		o = s.option(form.DummyValue, '_dest', _('组播地址'));
		o.modalonly = false;
		o.textvalue = function(sid) {
			var p = (uci.get('m3u8mc', sid, 'protocol') || 'rtp') + '://';
			return p + (uci.get('m3u8mc', sid, 'addr') || '?') + ':' + (uci.get('m3u8mc', sid, 'port') || '?');
		};

		o = s.option(form.DummyValue, '_mode', _('模式'));
		o.modalonly = false;
		o.textvalue = function(sid) {
			var dl = uci.get('m3u8mc', sid, 'use_downloader') !== '0';
			if (!dl) return _('直通');
			if (uci.get('m3u8mc', sid, 'proxy_only') === '1') return _('HTTP 代理');
			var t = _('缓存下载');
			if (uci.get('m3u8mc', sid, 'race_mode') === '1') t += '+' + _('赛马');
			return t;
		};

		o = s.option(form.DummyValue, '_cache', _('缓存'));
		o.modalonly = false;
		o.textvalue = function(sid) {
			if (uci.get('m3u8mc', sid, 'use_downloader') === '0') return '—';
			return uci.get('m3u8mc', sid, 'cache_storage') === 'disk' ? _('本地目录') : _('内存');
		};

		/* —— 基本 —— */
		o = s.taboption('basic', form.Value, 'name', _('频道名称'));
		o.modalonly = true;
		o.rmempty = false;
		o.placeholder = _('如：澳视体育');

		o = s.taboption('basic', form.DynamicList, 'url', _('镜像源地址'),
			_('内容相同的 m3u8 地址，可填多个镜像。标准模式下轮询并自动故障切换；直通模式只用第一条。'));
		o.modalonly = true;
		o.placeholder = 'http://...';
		o.validate = function(sid, val) {
			return (val == null || val === '' || /^https?:\/\//i.test(val)) ? true : _('地址必须以 http:// 或 https:// 开头');
		};

		o = s.taboption('basic', form.Value, 'addr', _('组播地址'));
		o.modalonly = true;
		o.rmempty = false;
		o.datatype = 'ip4addr("nomask")';
		o.placeholder = '239.255.1.1';

		o = s.taboption('basic', form.Value, 'port', _('组播端口'));
		o.modalonly = true;
		o.rmempty = false;
		o.datatype = 'port';
		o.placeholder = '1234';

		o = s.taboption('basic', form.ListValue, 'protocol', _('协议'));
		o.modalonly = true;
		o.value('rtp', 'RTP');
		o.value('udp', 'UDP');
		o.default = 'rtp';

		o = s.taboption('basic', form.Value, 'iface', _('出口接口'),
			_('组播从哪个网络接口发出（逻辑接口名，如 lan），也可直接填 IP。'));
		o.modalonly = true;
		o.placeholder = 'lan';
		o.default = 'lan';
		uci.sections('network', 'interface', function(sec) {
			if (sec['.name'] !== 'loopback') o.value(sec['.name']);
		});

		o = s.taboption('basic', form.Flag, 'use_downloader', _('启用缓存下载器（推荐）'),
			_('先把分片下载到本机缓存再推流，更稳、支持多镜像。取消勾选为“直通”：ffmpeg 直接读取第一条地址。'));
		o.modalonly = true;
		o.default = o.enabled = '1';
		o.rmempty = false;

		o = s.taboption('basic', form.Flag, 'proxy_only', _('仅 HTTP 代理（不推组播）'),
			_('只做下载加速和缓存，通过内部 HTTP 地址给播放器使用，不启动 ffmpeg。'));
		o.modalonly = true;
		o.default = '0';
		o.depends('use_downloader', '1');

		o = s.taboption('basic', form.Flag, 'race_mode', _('赛马下载'),
			_('同一分片同时向多个节点请求，谁先完成用谁。会成倍占用带宽，仅在源站/CDN 节点速度不稳时使用。'));
		o.modalonly = true;
		o.default = '0';
		o.depends('use_downloader', '1');

		/* —— 源与下载 —— */
		o = s.taboption('download', form.DynamicList, 'list_url', _('列表专用地址（可选）'),
			_('单独用来拉取 m3u8 列表的地址；不填则用上面的镜像源地址。配置后，分片下载地址只由“镜像源地址”决定。'));
		o.modalonly = true;
		o.depends('use_downloader', '1');

		o = s.taboption('download', form.DynamicList, 'race_ip', _('赛马节点'),
			_('每行一个：IP，或 IP@域名（用该域名作 Host 头）。不填则使用镜像源地址作为候选。'));
		o.modalonly = true;
		o.placeholder = '1.2.3.4  或  5.6.7.8@cdn.example.com';
		o.depends('race_mode', '1');

		o = s.taboption('download', form.Value, 'race_host', _('赛马全局 Host'),
			_('所有赛马节点统一使用的 Host 头，如 cdn.example.com 或 cdn.example.com:8443。'));
		o.modalonly = true;
		o.depends('race_mode', '1');

		o = s.taboption('download', form.Value, 'threads', _('分块下载线程数'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '4';
		o.default = '4';
		o.depends('use_downloader', '1');

		o = s.taboption('download', form.Value, 'list_poll_interval', _('列表轮询间隔（秒）'),
			_('0 表示按分片时长自适应。'));
		o.modalonly = true;
		o.datatype = 'ufloat';
		o.placeholder = '0';
		o.default = '0';
		o.depends('use_downloader', '1');

		o = s.taboption('download', form.Value, 'list_proxy', _('列表拉取代理'));
		o.modalonly = true;
		o.placeholder = 'http://127.0.0.1:7890';
		o.depends('use_downloader', '1');

		o = s.taboption('download', form.Value, 'download_proxy', _('分片下载代理'));
		o.modalonly = true;
		o.placeholder = 'socks5://127.0.0.1:1080';
		o.depends('use_downloader', '1');

		/* —— 缓存 —— */
		o = s.taboption('cache', form.ListValue, 'cache_storage', _('缓存位置'),
			_('内存：最快但占用 RAM；本地目录：每个分片存成一个文件，适合内存小、挂了 USB/SD/硬盘的设备。'));
		o.modalonly = true;
		o.value('memory', _('内存'));
		o.value('disk', _('本地目录'));
		o.default = 'memory';
		o.depends('use_downloader', '1');

		o = s.taboption('cache', form.Value, 'cache_dir', _('缓存目录'),
			_('本频道的磁盘缓存根目录，留空使用全局设置。目录不可写时自动回退到内存。'));
		o.modalonly = true;
		o.placeholder = '/mnt/sda1/m3u8mc';
		o.depends('cache_storage', 'disk');

		o = s.taboption('cache', form.Value, 'max_segments', _('最大缓存分片数'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '5';
		o.default = '5';
		o.depends('use_downloader', '1');

		o = s.taboption('cache', form.Value, 'cache_window', _('缓存窗口倍数'),
			_('缓存保留时长 = 分片时长 × 该倍数。'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '3';
		o.default = '3';
		o.depends('use_downloader', '1');

		o = s.taboption('cache', form.Value, 'hls_segments', _('播放列表分片数'),
			_('内部 HTTP 播放列表一次暴露的分片个数（预热分片数）。'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '3';
		o.default = '3';
		o.depends('use_downloader', '1');

		o = s.taboption('cache', form.Value, 'http_port', _('内部 HTTP 端口'),
			_('留空自动分配。'));
		o.modalonly = true;
		o.datatype = 'port';
		o.depends('use_downloader', '1');

		o = s.taboption('cache', form.Value, 'http_path', _('内部 HTTP 路径'));
		o.modalonly = true;
		o.placeholder = '/live';
		o.default = '/live';
		o.depends('use_downloader', '1');


		/* —— ffmpeg（结构化参数；额外参数列表追加在它们之后） —— */
		function ffOpt(type, key, title, desc, extra) {
			var o = s.taboption('ffmpeg', type, key, title, desc);
			o.modalonly = true;
			if (extra) extra(o);
			return o;
		}
		/* “留空 = 不设置该参数”：留空时写入空串，而不是删除选项（删除会回到内置默认值） */
		function ffBlankable(o) { o.optional = true; o.rmempty = false; }

		o = ffOpt(form.Flag, 'ffmpeg_re', _('按原始速率读取（-re）'),
			_('避免读取过快导致推流速率失控；本地缓存模式的输入是 HLS 时的标准做法。'),
			function(o) { o.default = o.enabled = '1'; });

		o = ffOpt(form.ListValue, 'ffmpeg_input_format', _('输入格式（-f）'),
			_('缓存下载器模式的输入固定是 HLS，选“hls”可避免 ffmpeg 自动探测误判；直通模式请保持自动探测。'),
			function(o) { o.value('', _('自动探测')); o.value('hls', 'hls'); o.default = ''; });

		o = ffOpt(form.Value, 'ffmpeg_analyzeduration', _('探测时长上限（-analyzeduration，微秒）'),
			_('调大可避免部分源探测不准导致丢帧/花屏。'),
			function(o) { o.datatype = 'uinteger'; o.placeholder = '5000000'; o.default = '5000000'; });

		o = ffOpt(form.Value, 'ffmpeg_probesize', _('探测数据量上限（-probesize，字节）'),
			_('配合探测时长上限使用。'),
			function(o) { o.datatype = 'uinteger'; o.placeholder = '5000000'; o.default = '5000000'; });

		o = ffOpt(form.Flag, 'ffmpeg_copy', _('直接复制音视频流（-c copy）'),
			_('强烈建议保持勾选。取消后 ffmpeg 会重新编码，路由器 CPU 基本扛不住。'),
			function(o) { o.default = o.enabled = '1'; });

		o = ffOpt(form.Flag, 'ffmpeg_annexb', _('H.264 转 Annex B（-bsf:v h264_mp4toannexb）'),
			_('仅当源是 mp4 风格 H.264 时才需要；HLS/TS 源一般不用。'),
			function(o) { o.default = '0'; });

		o = ffOpt(form.Value, 'ffmpeg_pcr_period', _('PCR 间隔（-pcr_period，毫秒）'),
			_('减小可改善电视盒子的时钟同步。留空=不设置此参数。'),
			function(o) { o.datatype = 'uinteger'; o.placeholder = '20'; o.default = '20'; ffBlankable(o); });

		o = ffOpt(form.Value, 'ffmpeg_muxdelay', _('复用延迟（-muxdelay，秒）'),
			_('降低直播端到端延迟。留空=不设置此参数。'),
			function(o) { o.datatype = 'ufloat'; o.placeholder = '0.7'; o.default = '0.7'; ffBlankable(o); });

		o = ffOpt(form.Value, 'ffmpeg_muxrate', _('固定输出码率（-muxrate）'),
			_('略高于源码率可稳定接收端缓冲，如 6M。留空=不固定码率。'),
			function(o) { o.placeholder = '6M'; o.validate = function(sid, v) {
				return (v == null || v === '' || /^\d+(\.\d+)?[kKmM]?$/.test(v)) ? true : _('格式如 6M、800k 或纯数字'); }; });

		o = ffOpt(form.Value, 'ffmpeg_pat_period', _('PAT 间隔（-pat_period，秒）'),
			_('PAT 表插入间隔，配合“重发头部”标志兜底。留空=不设置此参数。'),
			function(o) { o.datatype = 'ufloat'; o.placeholder = '0.5'; o.default = '0.5'; ffBlankable(o); });

		o = ffOpt(form.Value, 'ffmpeg_sdt_period', _('SDT 间隔（-sdt_period，秒）'),
			_('SDT 表插入间隔。留空=不设置此参数。'),
			function(o) { o.datatype = 'ufloat'; o.placeholder = '0.5'; o.default = '0.5'; ffBlankable(o); });

		o = ffOpt(form.Value, 'ffmpeg_service_type', _('服务类型（-mpegts_service_type）'),
			_('标记为数字电视可让电视盒子正确识别频道类型。可选预设，也可手填；留空=不设置。'),
			function(o) {
				['digital_tv', 'digital_radio', 'mpeg2_digital_hdtv', 'advanced_codec_digital_hdtv', 'hevc_digital_hdtv'].forEach(function(v) { o.value(v); });
				o.default = 'digital_tv'; ffBlankable(o);
			});

		o = ffOpt(form.Value, 'ffmpeg_mpegts_flags', _('封装标志（-mpegts_flags）'),
			_('+initial_discontinuity 在流开始时标记不连续点，+resend_headers 重发 PAT/PMT，换台更快。留空=不设置。'),
			function(o) {
				o.value('+initial_discontinuity+resend_headers');
				o.value('+resend_headers');
				o.default = '+initial_discontinuity+resend_headers'; ffBlankable(o);
			});

		o = ffOpt(form.DynamicList, 'ffmpeg_input_arg', _('额外输入参数'),
			_('追加在上面的输入参数之后、-i 之前，每项一个词（如先填 -extension_picky，再另起一项填 0）。用于补充上面没有的选项；同名选项以后出现者为准。'),
			function(o) { o.placeholder = '-extension_picky'; });

		o = ffOpt(form.DynamicList, 'ffmpeg_output_arg', _('额外输出参数'),
			_('追加在上面的输出参数之后。每项一个词。'),
			function(o) { o.placeholder = '-bsf:v'; });

		/* —— 高级 —— */
		o = s.taboption('advanced', form.Value, 'ttl', _('组播 TTL'),
			_('经过路由器转发的多跳网络需要调大。'));
		o.modalonly = true;
		o.datatype = 'range(1,255)';
		o.placeholder = '4';
		o.default = '4';

		o = s.taboption('advanced', form.Value, 'pkt_size', _('UDP 包大小'),
			_('一般不用改，在 MTU 允许范围内尽量大效率更高。'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '1316';
		o.default = '1316';

		o = s.taboption('advanced', form.Value, 'buffer_size', _('推流套接字缓冲（字节）'),
			_('写入输出地址的 buffer_size。内核会把它截断到 net.core.wmem_max，设置很大的值（如 40MB）时需要同时执行 sysctl -w net.core.wmem_max=… 才会真正生效。'));
		o.modalonly = true;
		o.datatype = 'uinteger';
		o.placeholder = '1048576';
		o.default = '1048576';

		o = s.taboption('advanced', form.ListValue, 'ffmpeg_loglevel', _('ffmpeg 日志级别'),
			_('传给 ffmpeg 的 -loglevel，调低可从源头减少输出（仅影响本频道）。“仅错误/仅致命”下正常运行的 ffmpeg 基本没有日志，因此会自动关闭“日志长时间静默则重启”的卡死检测；“仅致命”还会让依赖日志的错误关键字重启基本失效，只剩进程退出才会重启。修改后该频道会重启。'));
		o.modalonly = true;
		o.value('warning', _('警告（默认）'));
		o.value('error', _('仅错误'));
		o.value('fatal', _('仅致命'));
		o.value('info', _('信息（排障）'));
		o.default = 'warning';

		o = s.taboption('advanced', form.Flag, 'ffmpeg_log_forward', _('转发 ffmpeg 日志到运行日志'),
			_('勾选时 ffmpeg 的每一行输出会以调试级别写入运行日志（全局日志级别选“调试”时可见）；取消后不再写入，但 daemon 仍会读取并检查这些输出以便发现错误并重启，不影响稳定性。修改后该频道会重启。'));
		o.modalonly = true;
		o.default = o.enabled = '1';
		o.rmempty = false;

		o = s.taboption('advanced', form.DynamicList, 'http_header', _('公共 HTTP 请求头'),
			_('格式：Header: value'));
		o.modalonly = true;
		o.placeholder = 'User-Agent: Mozilla/5.0';

		o = s.taboption('advanced', form.DynamicList, 'poller_http_header', _('列表请求专属头'));
		o.modalonly = true;
		o.placeholder = 'Referer: https://list.example.com/';
		o.depends('use_downloader', '1');

		o = s.taboption('advanced', form.DynamicList, 'downloader_http_header', _('分片下载专属头'));
		o.modalonly = true;
		o.placeholder = 'Referer: https://cdn.example.com/';
		o.depends('use_downloader', '1');

		/* ───────── 快速添加（批量） ───────── */
		s.handleAdd = function(ev) {
			var map = this.map;
			var existing = {};
			var maxPort = 5003, lastAddr = '239.255.1.1';
			uci.sections('m3u8mc', 'channel', function(sec) {
				existing[sec['.name']] = true;
				var p = parseInt(sec.port, 10);
				if (p > maxPort) { maxPort = p; }
				if (sec.addr) lastAddr = sec.addr;
			});

			var ta = E('textarea', {
				'class': 'cbi-input-textarea', 'rows': 7, 'style': 'width:100%; font-family:monospace',
				'placeholder': '每行一个频道，格式：名称,m3u8地址\n例如：\n澳视体育,http://example.com/live/aomen.m3u8\nhttp://example.com/live/other.m3u8   （只写地址则以“频道N”命名）'
			});
			var addrIn = E('input', { 'type': 'text', 'class': 'cbi-input-text', 'value': lastAddr, 'style': 'width:12em' });
			var portIn = E('input', { 'type': 'number', 'class': 'cbi-input-text', 'value': maxPort + 1, 'min': 1, 'max': 65535, 'style': 'width:7em' });
			var chkDl = E('input', { 'type': 'checkbox', 'checked': 'checked', 'id': 'qa-dl' });
			var chkDisk = E('input', { 'type': 'checkbox', 'id': 'qa-disk' });
			var chkEn = E('input', { 'type': 'checkbox', 'checked': 'checked', 'id': 'qa-en' });
			var chkProxy = E('input', { 'type': 'checkbox', 'id': 'qa-proxy' });

			function row(label, ctl) { return E('div', { 'style': 'margin:.4em 0' }, [ E('label', {}, [ ctl, ' ', label ]) ]); }

			ui.showModal(_('快速添加频道'), [
				E('p', {}, _('一次添加多个频道：组播地址固定，端口从起始端口开始依次 +1。其余参数可在添加后点“编辑”细调。')),
				ta,
				E('div', { 'style': 'margin:.6em 0' }, [
					E('span', {}, _('组播地址：')), addrIn, '　',
					E('span', {}, _('起始端口：')), portIn
				]),
				row(_('启用缓存下载器（推荐）'), chkDl),
				row(_('缓存放在本地目录（需先在全局设置里指定磁盘缓存根目录）'), chkDisk),
				row(_('仅 HTTP 代理（不推组播）'), chkProxy),
				row(_('创建后立即启用'), chkEn),
				E('div', { 'class': 'right' }, [
					E('button', { 'class': 'btn cbi-button', 'click': ui.hideModal }, _('取消')), ' ',
					E('button', {
						'class': 'btn cbi-button cbi-button-positive important',
						'click': ui.createHandlerFn(this, function() {
							var lines = ta.value.split(/\r?\n/).map(function(l) { return l.trim(); }).filter(Boolean);
							var items = [], bad = [];
							lines.forEach(function(l) {
								var mm = l.match(/^(?:(.*?)\s*[,，\t]\s*|(\S.*?)\s+)?(https?:\/\/\S+)$/i);
								if (!mm) { bad.push(l); return; }
								items.push({ name: (mm[1] || mm[2] || '').trim(), url: mm[3] });
							});
							if (bad.length) {
								ui.addNotification(null, E('p', _('这些行无法识别（需要以 http:// 或 https:// 开头的地址）：') + bad.join(' ; ')), 'danger');
								return;
							}
							if (!items.length) {
								ui.addNotification(null, E('p', _('请至少输入一行频道。')), 'danger');
								return;
							}
							var addr = addrIn.value.trim();
							if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(addr)) {
								ui.addNotification(null, E('p', _('组播地址格式不正确。')), 'danger');
								return;
							}
							var port = parseInt(portIn.value, 10) || 1234;

							items.forEach(function(it, i) {
								var id = nextFreeId(existing);
								existing[id] = true;
								map.data.add('m3u8mc', 'channel', id);
								map.data.set('m3u8mc', id, 'name', it.name || (_('频道') + (i + 1)));
								map.data.set('m3u8mc', id, 'url', [ it.url ]);
								map.data.set('m3u8mc', id, 'addr', addr);
								map.data.set('m3u8mc', id, 'port', String(port + i));
								map.data.set('m3u8mc', id, 'enabled', chkEn.checked ? '1' : '0');
								map.data.set('m3u8mc', id, 'use_downloader', chkDl.checked ? '1' : '0');
								if (chkDl.checked && chkProxy.checked) map.data.set('m3u8mc', id, 'proxy_only', '1');
								if (chkDl.checked && chkDisk.checked) map.data.set('m3u8mc', id, 'cache_storage', 'disk');
							});
							ui.hideModal();
							return map.save(null, true);
						})
					}, _('添加'))
				])
			]);
		};

		return m.render();
	}
});
