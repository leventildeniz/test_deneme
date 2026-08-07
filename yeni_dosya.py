# yeni_dosya.py
def sayilari_topla(sayi1, sayi2):
    """İki sayıyı toplayıp sonucunu konsola yazdıran fonksiyon."""
    sonuc = sayi1 + sayi2
    print(f"{sayi1} + {sayi2} = {sonuc}")
    return sonuc

# Fonksiyonu test edelim
if __name__ == "__main__":
    sayilari_topla(10, 20)
