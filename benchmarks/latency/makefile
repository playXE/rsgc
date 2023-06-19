all: build run

CMD = ''


build:
	cd rust && cargo build --release 
	cd csharp && dotnet build -c Release
	cd java && javac Main.java

run:
	@echo "Running latency benchmark"
	@echo "RSGC (512MB heap)"
	@cd rust && GC_MAX_HEAP_SIZE=512M GC_PARALLEL_ROOT_MARKS=0 cargo run --release --quiet
	@echo "C# (default heap)"
	@cd csharp && dotnet run -c Release
	@echo "Java (512MB heap, Shenandoah)"
	@cd java && java -Xmx512M -XX:+UnlockExperimentalVMOptions -XX:+UseShenandoahGC Main
	@echo "Java (512MB heap, G1)"
	@cd java && java -Xmx512M -XX:+UnlockExperimentalVMOptions -XX:+UseG1GC Main
	@echo "Java (512MB heap, Z)"
	@cd java && java -Xmx512M -XX:+UnlockExperimentalVMOptions -XX:+UseZGC Main
	@echo "Java (512MB heap, Parallel)"
	@cd java && java -Xmx512M -XX:+UnlockExperimentalVMOptions -XX:+UseParallelGC Main
	@echo "Java (512MB heap, Serial)"
	@cd java && java -Xmx512M -XX:+UnlockExperimentalVMOptions -XX:+UseSerialGC Main
	@echo "Go (default heap)"
	@cd go && go run main.go