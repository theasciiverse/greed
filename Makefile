# Makefile for Greed
#
# SPDX-FileCopyrightText: (C) Eric S. Raymond <esr@thyrsus.com>
# SPDX-License-Identifier: BSD-2-Clause

PREFIX      ?= /usr/local
BINDIR      ?= $(PREFIX)/bin
DATADIR     ?= $(PREFIX)/share
MANDIR      ?= $(DATADIR)/man

SFILE=/usr/games/lib/greed.hs

CARGO ?= cargo
CARGOFLAGS ?= --release

VERSION=$(shell sed -n <NEWS.adoc '/^[0-9]/s/:.*//p' | head -1)

# Rules

# Note: to suppress the footers with timestamps being generated in HTML,
# we use "-a nofooter".
# To debug asciidoc problems, you may need to run "xmllint --nonet --noout --valid"
# on the intermediate XML that throws an error.

.SUFFIXES: .html .adoc .6

.adoc.6:
	asciidoctor -D. -a nofooter -b manpage $<
.adoc.html:
	asciidoctor -D. -a nofooter -a webfonts! $<

.PHONY: all clean reflow check spellcheck install uninstall dist release refresh

# Build

all: greed greed.6

greed: greed.rs Cargo.toml Cargo.lock
	SCOREFILE="$(SFILE)" RELEASE="$(VERSION)" $(CARGO) build $(CARGOFLAGS)
	cp target/release/greed greed

clean:
	rm -f *~ *.o greed greed-*.tar.gz  greed*.rpm *.html
	rm -f greed.6 manpage.links manpage.refs
	rm -rf target

# Validate

reflow:
	@$(CARGO) fmt

check:
	@SCOREFILE="$(SFILE)" RELEASE="$(VERSION)" $(CARGO) check $(CARGOFLAGS)

spellcheck:
	@spellcheck greed.adoc

# Install/uninstall

install: greed greed.6 uninstall
	install -m 0755 -d $(DESTDIR)$(BINDIR)
	install -m 0755 greed $(DESTDIR)$(BINDIR)/
	install -m 0755 -d $(DESTDIR)$(MANDIR)
	install -m 0755 -d $(DESTDIR)$(MANDIR)/man6
	install -m 0644 greed.6 $(DESTDIR)$(MANDIR)/man6/

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/greed
	rm -f $(DESTDIR)$(MANDIR)/man6/greed.6

# Export

SOURCES = README.adoc NEWS.adoc COPYING Makefile Cargo.toml Cargo.lock greed.rs greed.adoc control greed-logo.png

greed-$(VERSION).tar.gz: $(SOURCES)
	mkdir greed-$(VERSION)
	cp -r $(SOURCES) greed-$(VERSION)
	tar -czf greed-$(VERSION).tar.gz greed-$(VERSION)
	rm -fr greed-$(VERSION)
	ls -l greed-$(VERSION).tar.gz

dist: greed-$(VERSION).tar.gz

release: greed-$(VERSION).tar.gz greed.html
	@cargo_version=$$(sed -n 's/^version = "\(.*\)"$$/\1/p' Cargo.toml); test "$(VERSION)" = "$$cargo_version" || { echo "VERSION=$(VERSION) does not match Cargo.toml version=$$cargo_version" >&2; exit 1; }
	shipper version=$(VERSION) | sh -e -x

refresh: greed.html
	@cargo_version=$$(sed -n 's/^version = "\(.*\)"$$/\1/p' Cargo.toml); test "$(VERSION)" = "$$cargo_version" || { echo "VERSION=$(VERSION) does not match Cargo.toml version=$$cargo_version" >&2; exit 1; }
	shipper -N -w version=$(VERSION) | sh -e -x

# end
