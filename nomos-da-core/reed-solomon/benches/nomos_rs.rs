use criterion::{black_box, criterion_group, criterion_main, Criterion};
use reed_solomon::{decode_from_elements, encode_elements};

fn nomos_rs_encode(c: &mut Criterion) {
    for size in [1048576, 2097152, 4194304, 8388608, 16777216, 33554432] {
        let data = vec![1u8; size];
        let parity = 2;
        c.bench_function(&format!("nomos rs encode {}", size), |b| {
            b.iter(|| {
                black_box(encode_elements(parity, &data).unwrap());
            })
        });
    }
}

criterion_group!(benches, nomos_rs_encode);
criterion_main!(benches);
