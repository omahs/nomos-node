use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kzg::{types::kzg_settings::FsKZGSettings, utils::generate_trusted_setup};
use kzg_traits::{FFTSettings, KZGSettings};
use nomos_kzg::{compute_commitment, compute_proofs, verify_blob, Blob, KzgSettings};

fn create_kzg_settings(n: usize, secret: [u8; 32]) -> KzgSettings {
    let (g1s, g2s) = generate_trusted_setup(n, secret);
    let fft_settings = kzg::types::fft_settings::FsFFTSettings::new(8).unwrap();
    let settings = FsKZGSettings::new(&g1s, &g2s, n, &fft_settings).unwrap();
    KzgSettings {
        settings,
        bytes_per_field_element: 32,
    }
}

fn nomos_dynamic_vs_external(c: &mut Criterion) {
    let kzg_settings = create_kzg_settings(4096, [0; 32]);
    let blob = Blob::from_bytes(&[5; 4096 * 32], &kzg_settings).unwrap();

    let mut group = c.benchmark_group("KZG Commitment Benchmarks");

    group.bench_function("nomos blob commitment", |b| {
        b.iter(|| {
            nomos_kzg::blob_to_kzg_commitment(
                black_box(&blob),
                black_box(&kzg_settings.settings),
                black_box(4096),
            )
        })
    });

    group.bench_function("external blob commitment", |b| {
        b.iter(|| {
            kzg::eip_4844::blob_to_kzg_commitment_rust(
                black_box(&blob.inner()),
                black_box(&kzg_settings.settings),
            )
        })
    });

    group.finish();
}

fn nomos_different_blob_sizes(c: &mut Criterion) {
    let kzg_settings = create_kzg_settings(4096, [0; 32]);

    for size in (1..=32).map(|x| x * 1024 * 1024) {
        let blob = vec![0; size];

        let bench_name = format!("compute_{}_mb", size / (1024 * 1024));
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                let commitment =
                    compute_commitment(&black_box(blob.clone()), &kzg_settings).unwrap();
                let proofs =
                    compute_proofs(&black_box(blob.clone()), &commitment, &kzg_settings).unwrap();
                for proof in proofs {
                    assert!(verify_blob(
                        &black_box(blob.clone()),
                        &proof,
                        &commitment,
                        &kzg_settings
                    )
                    .unwrap());
                }
            })
        });
    }
}

fn nomos_commitment(c: &mut Criterion) {
    let kzg_settings = create_kzg_settings(4096, [0; 32]);
    for size in (1..=32).map(|x| x * 1024 * 1024) {
        let blob = vec![0; size];

        let bench_name = format!("compute_{}_mb", size / (1024 * 1024));
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                let _ = compute_commitment(&black_box(blob.clone()), &kzg_settings).unwrap();
            })
        });
    }
}

fn nomos_proof(c: &mut Criterion) {
    let kzg_settings = create_kzg_settings(4096, [0; 32]);
    for size in (1..=32).map(|x| x * 1024 * 1024) {
        let blob = vec![0; size];
        let commitment = compute_commitment(&black_box(blob.clone()), &kzg_settings).unwrap();

        let bench_name = format!("compute_{}_mb", size / (1024 * 1024));
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                _ = compute_proofs(&black_box(blob.clone()), &commitment, &kzg_settings).unwrap();
            })
        });
    }
}
fn nomos_verify(c: &mut Criterion) {
    let kzg_settings = create_kzg_settings(4096, [0; 32]);
    for size in (1..=32).map(|x| x * 1024 * 1024) {
        let blob = vec![0; size];
        let commitment = compute_commitment(&black_box(blob.clone()), &kzg_settings).unwrap();
        let proofs = compute_proofs(&black_box(blob.clone()), &commitment, &kzg_settings).unwrap();

        let bench_name = format!("compute_{}_mb", size / (1024 * 1024));
        c.bench_function(&bench_name, |b| {
            b.iter(|| {
                for proof in &proofs {
                    assert!(verify_blob(
                        &black_box(blob.clone()),
                        proof,
                        &commitment,
                        &kzg_settings
                    )
                    .unwrap());
                }
            })
        });
    }
}

criterion_group!(
    benches,
    nomos_dynamic_vs_external,
    nomos_different_blob_sizes,
    nomos_commitment,
    nomos_proof,
    nomos_verify,
);
criterion_main!(benches);
