.PHONY: rpm-al2023 rpm-f44 rpm-verify-al2023 rpm-verify-f44

rpm-al2023:
	./packaging/rpm/build-in-container.sh al2023

rpm-f44:
	./packaging/rpm/build-in-container.sh f44

rpm-verify-al2023:
	./packaging/rpm/verify-in-container.sh al2023

rpm-verify-f44:
	./packaging/rpm/verify-in-container.sh f44
