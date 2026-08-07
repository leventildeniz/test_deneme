# Rust API Fetcher

Bu proje, Rust dili kullanılarak geliştirilmiş basit bir API istemcisidir. `reqwest` ve `serde` kütüphanelerini kullanarak harici bir API'den veri çeker ve bu verileri yapılandırılmış bir şekilde ekrana yazdırır.

## Özellikler
- **Asenkron Yapı:** `tokio` runtime kullanılarak yüksek performanslı asenkron istekler.
- **JSON Serileştirme:** `serde` ile JSON verilerinin Rust struct yapılarına dönüştürülmesi.
- **HTTP İstekleri:** `reqwest` ile güvenilir API iletişimi.

## Kurulum ve Çalıştırma
Projeyi yerel makinenizde çalıştırmak için şu adımları izleyin:

1. Rust'ın yüklü olduğundan emin olun.
2. Proje dizinine gidin:
   ```bash
   cd .
   ```
3. Uygulamayı çalıştırın:
   ```bash
   cargo run
   ```

## Kullanılan Kütüphaneler
- `tokio`: Asenkron runtime.
- `reqwest`: HTTP istemcisi.
- `serde` & `serde_json`: Veri serileştirme ve deserializasyon.

---
*Forge IDE tarafından otomatik olarak oluşturulmuştur.*
