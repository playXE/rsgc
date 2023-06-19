const windowSize = 200_000;
const msgCount = 1_000_000;
const msgSize = 1024;

let worst = 0;

function createMessage(n) {
    const msg = new Uint8Array(msgSize);
    msg.fill(n);
    return msg;
}

function pushMessage(store, id) {
    const start = process.hrtime.bigint();
    store[id % windowSize] = createMessage(id);
    const elapsed = process.hrtime.bigint() - start;
    if (elapsed > worst) {
        worst = elapsed;
    }
}

const store = new Array(windowSize);
for (let i = 0; i < msgCount; i++) {
    pushMessage(store, i);
}
console.log(`Worst push time: ${worst / 1000_000n}ms`);