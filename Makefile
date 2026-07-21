.PHONY: validate bootstrap dev-secrets package
validate:
	./scripts/validate-package.py
	@for f in scripts/*.sh; do bash -n "$$f"; done
bootstrap:
	./scripts/bootstrap-example-config.sh
dev-secrets:
	./scripts/generate-dev-secrets.sh
package:
	./scripts/package.sh
