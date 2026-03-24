AUR_GIT_DIR ?= /tmp/scrollshot-git-aur
AUR_BIN_DIR ?= /tmp/scrollshot-bin-aur

.PHONY: srcinfo-git srcinfo-bin publish-git publish-bin

## Generate .SRCINFO for scrollshot-git
srcinfo-git:
	cd . && makepkg --printsrcinfo > .SRCINFO

## Generate .SRCINFO for scrollshot-bin
srcinfo-bin:
	cd bin && makepkg --printsrcinfo > .SRCINFO

## Clone AUR repo, copy PKGBUILD + .SRCINFO, commit and push scrollshot-git
publish-git: srcinfo-git
	git clone ssh://aur@aur.archlinux.org/scrollshot-git.git $(AUR_GIT_DIR) 2>/dev/null || true
	cp PKGBUILD .SRCINFO $(AUR_GIT_DIR)/
	cd $(AUR_GIT_DIR) && git add PKGBUILD .SRCINFO && git commit -m "Update to $$(grep pkgver PKGBUILD | head -1 | cut -d= -f2)" && git push

## Clone AUR repo, copy PKGBUILD + .SRCINFO, commit and push scrollshot-bin
publish-bin: srcinfo-bin
	git clone ssh://aur@aur.archlinux.org/scrollshot-bin.git $(AUR_BIN_DIR) 2>/dev/null || true
	cp bin/PKGBUILD bin/.SRCINFO $(AUR_BIN_DIR)/
	cd $(AUR_BIN_DIR) && git add PKGBUILD .SRCINFO && git commit -m "Update to $$(grep pkgver PKGBUILD | head -1 | cut -d= -f2)" && git push
