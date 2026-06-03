#[doc = "Register `blit` reader"]
pub type R = crate::R<BLIT_SPEC>;
#[doc = "Register `blit` writer"]
pub type W = crate::W<BLIT_SPEC>;
#[doc = "Field `dst_x` writer - dst_x field"]
pub type DST_X_W<'a, REG> = crate::FieldWriter<'a, REG, 12, u16>;
#[doc = "Field `dst_y` writer - dst_y field"]
pub type DST_Y_W<'a, REG> = crate::FieldWriter<'a, REG, 12, u16>;
#[doc = "Field `pixel` writer - pixel field"]
pub type PIXEL_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
impl W {
    #[doc = "Bits 0:11 - dst_x field"]
    #[inline(always)]
    pub fn dst_x(&mut self) -> DST_X_W<'_, BLIT_SPEC> {
        DST_X_W::new(self, 0)
    }
    #[doc = "Bits 12:23 - dst_y field"]
    #[inline(always)]
    pub fn dst_y(&mut self) -> DST_Y_W<'_, BLIT_SPEC> {
        DST_Y_W::new(self, 12)
    }
    #[doc = "Bits 24:31 - pixel field"]
    #[inline(always)]
    pub fn pixel(&mut self) -> PIXEL_W<'_, BLIT_SPEC> {
        PIXEL_W::new(self, 24)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`blit::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`blit::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct BLIT_SPEC;
impl crate::RegisterSpec for BLIT_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`blit::R`](R) reader structure"]
impl crate::Readable for BLIT_SPEC {}
#[doc = "`write(|w| ..)` method takes [`blit::W`](W) writer structure"]
impl crate::Writable for BLIT_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets blit to value 0"]
impl crate::Resettable for BLIT_SPEC {}
