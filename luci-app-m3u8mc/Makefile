include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-m3u8mc
PKG_VERSION:=0.1.0
PKG_RELEASE:=2
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
	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/controller
	$(INSTALL_DATA) ./luci/controller/m3u8mc.lua \
	    $(1)/usr/lib/lua/luci/controller/m3u8mc.lua

	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/model/cbi/m3u8mc
	$(INSTALL_DATA) ./luci/model/cbi/m3u8mc/channels.lua \
	    $(1)/usr/lib/lua/luci/model/cbi/m3u8mc/channels.lua
	$(INSTALL_DATA) ./luci/model/cbi/m3u8mc/channel_edit.lua \
	    $(1)/usr/lib/lua/luci/model/cbi/m3u8mc/channel_edit.lua

	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/view/m3u8mc
	$(INSTALL_DATA) ./luci/view/m3u8mc/status.htm \
	    $(1)/usr/lib/lua/luci/view/m3u8mc/status.htm
	$(INSTALL_DATA) ./luci/view/m3u8mc/log.htm \
	    $(1)/usr/lib/lua/luci/view/m3u8mc/log.htm

	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./acl.d/luci-app-m3u8mc.json \
	    $(1)/usr/share/rpcd/acl.d/luci-app-m3u8mc.json

	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./menu.d/luci-app-m3u8mc.json \
	    $(1)/usr/share/luci/menu.d/luci-app-m3u8mc.json
endef

$(eval $(call BuildPackage,luci-app-m3u8mc))
