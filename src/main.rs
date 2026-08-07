// src/main.rs
use serde::Deserialize;
use reqwest::Error;

// API'den dönecek olan verinin yapısını tanımlıyoruz.
// #[derive(Deserialize)] sayesinde JSON verisi otomatik olarak bu struct'a dönüştürülür.
#[derive(Deserialize, Debug)]
struct Post {
    userId: i32,
    id: i32,
    title: String,
    body: String,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    // İstek atılacak URL
    let url = "https://jsonplaceholder.typicode.com/posts/1";

    println!("Veri çekiliyor: {}...", url);

    // 1. GET isteği gönderiyoruz
    let response = reqwest::get(url).await?;

    // 2. İsteğin başarılı olup olmadığını kontrol ediyoruz (HTTP 200 OK vb.)
    if response.status().is_success() {
        // 3. Yanıtı JSON olarak parse edip Post struct'ına aktarıyoruz
        let post: Post = response.json().await?;
        
        println!("İstek Başarılı!");
        println!("Başlık: {}", post.title);
        println!("İçerik: {}", post.body);
        println!("Yazar ID: {}", post.userId);
    } else {
        println!("Bir hata oluştu. Durum kodu: {}", response.status());
    }

    Ok(())
}
