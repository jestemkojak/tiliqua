#[doc = "Register `h_timing2` reader"]
pub type R = crate::R<H_TIMING2_SPEC>;
#[doc = "Register `h_timing2` writer"]
pub type W = crate::W<H_TIMING2_SPEC>;
#[doc = "Field `h_sync_end` writer - h_sync_end field"]
pub type H_SYNC_END_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
#[doc = "Field `h_total` writer - h_total field"]
pub type H_TOTAL_W<'a, REG> = crate::FieldWriter<'a, REG, 16, u16>;
impl W {
    #[doc = "Bits 0:15 - h_sync_end field"]
    #[inline(always)]
    pub fn h_sync_end(&mut self) -> H_SYNC_END_W<'_, H_TIMING2_SPEC> {
        H_SYNC_END_W::new(self, 0)
    }
    #[doc = "Bits 16:31 - h_total field"]
    #[inline(always)]
    pub fn h_total(&mut self) -> H_TOTAL_W<'_, H_TIMING2_SPEC> {
        H_TOTAL_W::new(self, 16)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`h_timing2::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`h_timing2::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct H_TIMING2_SPEC;
impl crate::RegisterSpec for H_TIMING2_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`h_timing2::R`](R) reader structure"]
impl crate::Readable for H_TIMING2_SPEC {}
#[doc = "`write(|w| ..)` method takes [`h_timing2::W`](W) writer structure"]
impl crate::Writable for H_TIMING2_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets h_timing2 to value 0"]
impl crate::Resettable for H_TIMING2_SPEC {}
