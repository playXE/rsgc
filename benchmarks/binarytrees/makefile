all: build run

build:
	cd rust && cargo build --release
	cd csharp && dotnet build -c Release
	cd java && javac binarytrees.java 
run:
	hyperfine './rust/target/release/binarytrees 21' \
	'cd csharp && DOTNET_gcServer=1 dotnet run -c Release -- 21' \
	'cd csharp && DOTNET_gcServer=0 dotnet run -c Release -- 21' \
	'go run go/binarytrees.go 21' \
	'cd java && java -XX:+UseG1GC binarytrees 21' \
	'cd java && java -XX:+UseShenandoahGC binarytrees 21' \
	'cd java && java -XX:+UseZGC binarytrees 21' \
	'cd java && java -XX:+UseSerialGC binarytrees 21' \
	'go run go/binarytrees.go 21'

