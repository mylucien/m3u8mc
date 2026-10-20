include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-m3u8mc
PKG_VERSION:=0.1.0
PKG_RELEASE:=3
PKG_MAINTAINER:=wupengyan
PKG_LICENSE:=MIT

PKGARCH:=all

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-m3u8mc
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=LuCI support for m3u8mc
  DEPENDS:=+m3u8mc +luci-base
  PKGARCH:=all
endef

define Package/luci-app-m3u8mc/description
  LuCI management interface for m3u8mc: channel management,
  realtime log viewer, and status monitoring.
endef

define Build/Compile
	@true
endef

define Package/luci-app-m3u8mc/install
	$(INSTALL_DIR) $(1)/www/luci-static/resources/view/m3u8mc
	$(INSTALL_DATA) ./htdocs/luci-static/resources/view/m3u8mc/channels.js \
	    $(1)/www/luci-static/resources/view/m3u8mc/channels.js
	$(INSTALL_DATA) ./htdocs/luci-static/resources/view/m3u8mc/status.js \
	    $(1)/www/luci-static/resources/view/m3u8mc/status.js
	$(INSTALL_DATA) ./htdocs/luci-static/resources/view/m3u8mc/log.js \
	    $(1)/www/luci-static/resources/view/m3u8mc/log.js

	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-m3u8mc.json \
	    $(1)/usr/share/rpcd/acl.d/luci-app-m3u8mc.json

	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-m3u8mc.json \
	    $(1)/usr/share/luci/menu.d/luci-app-m3u8mc.json
endef

# 安装后清理 LuCI 缓存并让 rpcd 重新加载 ACL，否则新菜单/权限要重启后才生效
define Package/luci-app-m3u8mc/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] || {
	rm -f /tmp/luci-indexcache* 2>/dev/null
	rm -rf /tmp/luci-modulecache 2>/dev/null
	/etc/init.d/rpcd reload 2>/dev/null
}
exit 0
endef

$(eval $(call BuildPackage,luci-app-m3u8mc))
