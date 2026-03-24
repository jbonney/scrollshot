AUR_GIT_DIR ?= /tmp/scrollshot-git-aur
AUR_BIN_DIR ?= /tmp/scrollshot-bin-aur

.PHONY: srcinfo-git srcinfo-bin publish-git publish-bin

## Generate .SRCINFO for scrollshot-git
srcinfo-git:
	cd . && makepkg --printsrcinfo > .SRCINFO

## Generate .SRCINFO for scrollshot-bin
srcinfo-bin:
	cd bin && makepkg --printsrcinfo > .SRCINFO

## Clone/update AUR repo, copy PKGBUILD + .SRCINFO, commit and push scrollshot-git
publish-git: srcinfo-git
	@if [ -d $(AUR_GIT_DIR) ]; then cd $(AUR_GIT_DIR) && git pull; \
	else git clone ssh://aur@aur.archlinux.org/scrollshot-git.git $(AUR_GIT_DIR); fi
	cp PKGBUILD .SRCINFO $(AUR_GIT_DIR)/
	cd $(AUR_GIT_DIR) && git add PKGBUILD .SRCINFO && git commit -m "Update to $$(grep pkgver PKGBUILD | head -1 | cut -d= -f2)" && git push

## Clone/update AUR repo, copy PKGBUILD + .SRCINFO, commit and push scrollshot-bin
publish-bin: srcinfo-bin
	@if [ -d $(AUR_BIN_DIR) ]; then cd $(AUR_BIN_DIR) && git pull; \
	else git clone ssh://aur@aur.archlinux.org/scrollshot-bin.git $(AUR_BIN_DIR); fi
	cp bin/PKGBUILD bin/.SRCINFO $(AUR_BIN_DIR)/
	cd $(AUR_BIN_DIR) && git add PKGBUILD .SRCINFO && git commit -m "Update to $$(grep pkgver PKGBUILD | head -1 | cut -d= -f2)" && git push
