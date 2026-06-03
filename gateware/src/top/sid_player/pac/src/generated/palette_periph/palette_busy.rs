#[doc = "Register `palette_busy` reader"]
pub type R = crate::R<PALETTE_BUSY_SPEC>;
#[doc = "Register `palette_busy` writer"]
pub type W = crate::W<PALETTE_BUSY_SPEC>;
#[doc = "Field `busy` reader - busy field"]
pub type BUSY_R = crate::BitReader;
impl R {
    #[doc = "Bit 0 - busy field"]
    #[inline(always)]
    pub fn busy(&self) -> BUSY_R {
        BUSY_R::new((self.bits & 1) != 0)
    }
}
impl W {}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`palette_busy::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`palette_busy::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct PALETTE_BUSY_SPEC;
impl crate::RegisterSpec for PALETTE_BUSY_SPEC {
    type Ux = u8;
}
#[doc = "`read()` method returns [`palette_busy::R`](R) reader structure"]
impl crate::Readable for PALETTE_BUSY_SPEC {}
#[doc = "`write(|w| ..)` method takes [`palette_busy::W`](W) writer structure"]
impl crate::Writable for PALETTE_BUSY_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets palette_busy to value 0"]
impl crate::Resettable for PALETTE_BUSY_SPEC {}
